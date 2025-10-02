# 生成→学習→ログ→ガントレット — ハンズオン手順

本書は、運用改善トラック（`docs/10_pipeline.md`）の主動線である「生成→学習→ログ→ガントレット」を、最短で一周回すための実行手順をまとめたハンズオンガイドです。固定条件・Gate は `docs/00_charter.md` に従います。

## 前提と準備
- ビルド: `cargo build -p tools`（大規模運用は `--release` 推奨）
- 再現条件（必須）: `RAYON_NUM_THREADS=1`、`--threads 1`、`--hash-mb 256`、`--time "0/1+0.1"`
- フィクスチャ: 
  - 入力: `docs/reports/fixtures/psv_sample.psv`
  - 開幕ブック: `docs/reports/fixtures/opening/representative.epd`

---

## 1) 生成（教師データ作成）
SFEN 行を逐次読みし、探索で注釈（CP/WDL）を付与した学習データ（JSONL）を出力します。メモリピークはほぼ一定です。

例（小規模で流れ確認）
```bash
cargo run -p tools --bin generate_nnue_training_data -- \
  docs/reports/fixtures/psv_sample.psv runs/train.jsonl \
  --preset balanced --output-format jsonl --min-depth 3 --multipv 2 \
  --split 100000 --compress gz
```

ポイント
- `--preset baseline|balanced|high` は time/hash/multipv/min-depth を同時設定（CLI指定があれば優先）。
- `--split` と `--compress gz|zst` で出力を分割・圧縮（zst は `--features zstd` 必須）。
- `-`（STDIN）入力も可。進捗は `<out>.progress` に保存され自動レジューム。
- 出力の隣に `*.manifest.json`（v2）が生成され、実行要約と再現メタを保持。

生成物
- `runs/train.jsonl`（分割運用時は `runs/train.jsonl.part-0001.gz` など）
- `runs/train.jsonl.manifest.json` および `runs/train_skipped.jsonl`

---

## 2) 特徴キャッシュ（任意・高速化）
学習時の I/O と特徴抽出コストを削減します。JSONL 直接学習も可能ですが、大規模データは cache 推奨です。

```bash
cargo run -p tools --bin build_feature_cache -- \
  -i runs/train.jsonl -o runs/train.cache.gz
```

---

## 3) 学習（train_nnue）
`train_nnue` は Single/Classic 両アーキテクチャに対応しています。`--arch` を省略すると既定で Single を学習します。Classic へ切り替える場合は追加で `--arch classic` を指定し、必要に応じて蒸留やエクスポート形式を組み合わせます。

### 3.1 Classic in-memory（蒸留 + メトリクス/ログ出力）
JSONL を直接読み込むメモリ常駐モード。`--distill-from-single` に教師 FP32、`--export-format classic-v1` で Classic 用ヘッダを生成します。`--metrics` で `metrics.csv` を追記し、`--structured-log` で JSONL ログを残します。

```bash
cargo run -p tools --bin train_nnue --release -- \
  --input runs/train.jsonl \
  --arch classic \
  --distill-from-single runs/teacher.fp32.bin \
  --export-format classic-v1 \
  --epochs 2 --batch-size 8192 \
  --opt adamw --grad-clip 1.0 \
  --metrics \
  --structured-log runs/logs/classic_inmem.jsonl \
  --save-every 2000 \
  --out runs/classic_inmem
```

> 再現性を確保したい場合は `--rng-seed <u64>`（`--seed` も同義のエイリアス）を併用してください。Classic v1 をエクスポートしつつ FP32 重みと量子化スケール JSON を残したい場合は `--emit-fp32-also` を追加すると、同じ出力ディレクトリに `nn.fp32.bin` と `nn.classic.scales.json` が書き出されます。

> 注意: `--opt adamw` は Classic で decoupled weight decay に対応しています。一方 Single では Adam として動き、警告が出力されます。

### 3.2 Classic stream-cache（大規模データ向け）
特徴キャッシュを逐次読み込むモード。`--stream-cache` を有効にし、Prefetch と I/O を調整します。

```bash
cargo run -p tools --bin train_nnue --release -- \
  --input runs/train.cache.gz \
  --arch classic \
  --stream-cache --prefetch-batches 2 \
  --distill-from-single runs/teacher.fp32.bin \
  --export-format classic-v1 \
  --epochs 3 --batch-size 16384 \
  --lr-schedule cosine --lr-warmup-epochs 1 \
  --metrics --structured-log runs/logs/classic_stream.jsonl \
  --save-every 4000 \
  --out runs/classic_stream
```

> `--rng-seed`（別名 `--seed`）でストリーム読み込み時の RNG を固定できます。Classic v1 export と同時に FP32/スケールを残す場合は `--emit-fp32-also` を指定してください。

> `classic-v1` を stream-cache と組み合わせる場合は必ず蒸留を伴う必要があります（実装上、蒸留をスキップすると起動直後に明示的エラーで停止します）。蒸留を行わない学習では `--export-format` を `fp32` または未指定にしてください。

### 3.3 Single（従来フロー）
Single-channel のスモークテスト（1 epoch）。構造化ログは Classic と同様に利用できます。

```bash
cargo run -p tools --bin train_nnue -- \
  --input runs/train.cache.gz \
  --arch single \
  --stream-cache --prefetch-batches 2 \
  --epochs 1 --batch-size 8192 \
  --lr-schedule constant \
  --structured-log runs/logs/single_stream.jsonl
```

検証込みで Cosine スケジュールを使う例。

```bash
cargo run -p tools --bin train_nnue -- \
  --input runs/train.cache.gz \
  --validation runs/val.jsonl \
  --arch single \
  --epochs 3 --batch-size 16384 \
  --lr-schedule cosine --lr-warmup-epochs 1 \
  --structured-log runs/logs/single_val.jsonl
```

### 3.4 重み付け（任意）
サンプル重み付けを ON にする場合は Single/Classic 共通で指定可能です。

```bash
cargo run -p tools --bin train_nnue -- \
  --input runs/train.cache.gz \
  --arch classic \
  --stream-cache --prefetch-batches 2 \
  --epochs 2 --batch-size 16384 \
  --weighting exact --weighting phase \
  --w-exact 1.2 --w-phase-endgame 1.3 \
  --structured-log runs/logs/classic_weighted.jsonl
```

生成物（Single/Classic 共通）
- `runs/nnue_<ts>/nn.fp32.bin`（Classic の場合は `--export-format classic-v1` 指定で量子化済みバンドルも生成。Classic で `--emit-fp32-also` を付けると FP32 と `nn.classic.scales.json` が同時出力されます）
- `nn_best.fp32.bin`、`config.json`、必要に応じて各種 CSV/PNG（`--metrics` 有効時）
- 構造化ログ: `runs/logs/*.jsonl`

ログ確認（必須キー）
- `global_step, epoch, lr, train_loss, val_loss, val_auc, examples_sec, loader_ratio, wall_time`

---

## 4) ガントレット（昇格判定）
固定条件（`00_charter.md`）で勝率・NPS・PVスプレッドを測定し Gate を判定します。

まずはスタブで動線確認
```bash
RAYON_NUM_THREADS=1 cargo run -p tools --bin gauntlet -- \
  --base runs/nn/mock_base.nnue --cand runs/nn/mock_cand.nnue \
  --time "0/1+0.1" --games 20 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/representative.epd --multipv 1 \
  --json runs/gauntlet/out.json --report runs/gauntlet/report.md \
  > runs/gauntlet/structured.jsonl
```

実走（学習出力の重みで比較）
```bash
RAYON_NUM_THREADS=1 cargo run -p tools --bin gauntlet -- \
  --base <baseline.bin> --cand runs/nnue_*/nn_best.fp32.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/representative.epd --multipv 1 \
  --json runs/gauntlet/out.json --report runs/gauntlet/report.md \
  # 任意: PVスプレッド計測用の固定時間（既定は inc_ms, 最低100ms）\
  --pv-ms 300 \
  > runs/gauntlet/structured.jsonl
```

Gate（合否）
- 勝率 +5pt 以上（=55%）かつ NPS ±3% 以内
- PV スプレッド P90 は「ベースライン +30cp」を超えない

出力
- JSON: `runs/gauntlet/out.json`（環境/条件/要約/各ゲーム）
- Markdown: `runs/gauntlet/report.md`（人間可読の要約）
- Structured: `runs/gauntlet/structured.jsonl`（phase=gauntlet の1行）

---

## トラブルシュート
- 圧縮 `zst`: ツールを `--features zstd` でビルドして使用
- `loader_ratio` が高い: `--prefetch-batches` を増やす or cache 入力を使用
- 再現性: 学習は `--seed`、ガントレットは `--seed`/`--seed-mode`

---
