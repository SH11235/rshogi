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
SINGLE_CHANNEL 形式の NNUE を学習し、構造化ログ（structured v1）を出力します。

スモーク（1 epoch、構造化ログ出力）
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/train.cache.gz -e 1 -b 8192 \
  --stream-cache --prefetch-batches 2 \
  --lr-schedule constant \
  --structured-log runs/train_structured.jsonl
```

検証あり・Cosineスケジュール例
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/train.cache.gz -v runs/val.jsonl \
  -e 3 -b 16384 \
  --lr-schedule cosine --lr-warmup-epochs 1 \
  --structured-log runs/train_structured.jsonl
```

サンプル重み付け（Next: #12、任意）
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/train.cache.gz -e 2 -b 16384 \
  --weighting exact --weighting phase \
  --w-exact 1.2 --w-phase-endgame 1.3 \
  --structured-log runs/train_structured.jsonl
```

生成物
- `runs/nnue_<ts>/nn.fp32.bin`（SINGLE_CHANNEL 形式）
- 検証あり時: `nn_best.fp32.bin`、`config.json`、（任意で）各種 CSV/PNG
- 構造化ログ: `runs/train_structured.jsonl`

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
  --stub > runs/gauntlet/structured.jsonl
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

## 補足: Phase 1 との関係
- 本手順は「運用改善」側の流れです。Phase 1（`docs/20_engine.md`）は以下のいずれかで達成します。
  - Classic NNUE 合流（256×2→32→32→1, ClippedReLU）に統一
  - SINGLE_CHANNEL の差分更新を実装（暫定）し NPS を底上げ

実装ガイドの詳細は `docs/20_engine.md` の「Phase 1 実装ガイド（具体化）」を参照してください。
