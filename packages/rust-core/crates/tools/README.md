# ツール集（crates/tools）

このクレートには、将棋エンジン/NNUE 学習のためのユーティリティ群が含まれます。主運用は「JSONL 生成 → キャッシュ作成 → 学習 → 解析/抽出/マージ」です。

## ビルド

- ワークスペース全体: `cargo build --release`
- 本クレートの特定バイナリ: `cargo build --release --bin <ツール名>`

## 推奨ワークフロー（学習データ〜学習）

1) 教師データ生成（JSONL 推奨）
```bash
cargo run --release -p tools --bin generate_nnue_training_data -- \
  input.sfens runs/out_pass1.jsonl 2 100 \
  --engine enhanced \
  --output-format jsonl \
  --multipv 2 \
  --label wdl --wdl-scale 600 \
  --hash-mb 16
```
- 進捗/再開: `*.progress` によるレジューム対応。スキップは `*_skipped.*` に出力。
- 補足: Phase-2 では JSONL 出力と MultiPV≥2 を推奨。

2) 特徴キャッシュ作成（高速学習向け）
```bash
cargo run --release -p tools --bin build_feature_cache -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache \
  -l wdl --scale 600 --exclude-no-legal-move --exclude-fallback \
  --io-buf-mb 8 --metrics-interval 20000 --report-rss

# 圧縮付き（ペイロードのみ圧縮。ヘッダは非圧縮）
cargo run --release -p tools --bin build_feature_cache -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache \
  -l wdl --scale 600 --compress --compressor gz --compress-level 6
# zstd を使う場合（tools クレートを `--features zstd` でビルド）
cargo run --release -p tools --bin build_feature_cache --features zstd -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache -l wdl --compress --compressor zst --compress-level 10
```
 
  - v1 フォーマット（既定）では 2 サンプル/局面（先手視点・後手視点）に分割し、ラベルは黒基準で整合化します。
  - `--dedup-features` で特徴の重複を除去（デフォルトOFF）。統計に dedup の有無を表示。
  - 圧縮はヘッダ非圧縮＋ペイロード部のみ圧縮（gzip/zstd）。トレーナはヘッダの `payload_encoding` を自動判別して読込。
  - 圧縮時は `chunk_size` 件（= サンプル単位。1局面=先手/後手の2サンプル）ごとにメンバー/フレームを区切る（マルチメンバー gzip / 連結 zstd）。メモリピークの安定化に有効。
  - 入力 JSONL は `.jsonl`/`.jsonl.gz`/`.jsonl.zst`（zstdは feature 有効時）を自動判別。

3) NNUE 学習（キャッシュ入力推奨）
```bash
# キャッシュから学習（推奨。v1/圧縮対応）
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 2 -b 16384 --lr 0.001 --seed 42 -o runs/my_nnue

# JSONLからの学習（I/O負荷が高め）
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.jsonl -e 2 -b 8192 --lr 0.001 --seed 42 -o runs/my_nnue_jsonl

# 量子化モデルの保存
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 1 --quantized -o runs/my_nnue_q

# スループット表示と非同期プリフェッチ（キャッシュ入力時）
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 1 -b 16384 --prefetch-batches 4 --throughput-interval 2.0
  # => [throughput] sps(=samples/sec), bps(=batches/sec), avg_batch を定期表示
  #    prefetch-batches=0 を指定すると同期モード（mode=sync）で動作

## ストリーミング学習モード（大規模/圧縮キャッシュ向け）

学習前に全量をメモリ化せず、キャッシュをバックグラウンドで逐次読み込みます。
シャッフルは無効化（現状）。ローダ待ち比率（loader_ratio）と sps の改善を比較できます。

```bash
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 1 -b 16384 \
  --stream-cache --prefetch-batches 4 --throughput-interval 2.0
# ログ: [throughput] mode=stream ... loader_ratio=...%
```

オプション補足:
- `--prefetch-batches N`: stream-cache / cache 入力時のプリフェッチ深さ。
- `--prefetch-bytes BYTES`: プリフェッチの概算メモリ上限（バイト）。0 または未指定で無制限。
- `--estimated-features-per-sample N`: サンプル1件あたりの推定活性特徴数（既定 64）。
  - 概算メモリは `~32 + 4*N` バイト/サンプルとして見積もられ、`--prefetch-bytes` の丸めに使用されます。
  - 実データで活性数が多い場合は N を増やすと安全です。

ログの意味:
- `[throughput] mode=stream ... loader_ratio=...%` は、非同期ローダに対する受信待機（I/O/解凍待ち等）が占める割合です。
- in‑memory 経路は `mode=inmem loader=async|sync` として出力され、`loader_ratio` は概ね 0% になります。
```

4) 品質解析（ゲート/要約/複数入力の比較）
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/out_pass1.jsonl --summary --report exact-rate --report gap2 \
  --gate '{"exact_top1_min":0.98,"exact_both_min":0.90}' --gate-mode warn

# 複数入力の比較 + 重複除去
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/p1.jsonl --inputs runs/p2.jsonl --dedup-by-sfen --summary
```
- 入力圧縮: `.gz`は標準対応。`.zst`は tools の `zstd` feature 有効時に対応（未有効時は実行時に警告）。

5) 抽出/マージ（パイプライン連携）
```bash
# 曖昧局面などの抽出（gapしきい値/非EXACT/aspiration失敗等）
# 注: `--gap-threshold` / `--max-gap-cp` は「以下を拾う」条件（best2_gap_cp <= しきい値）。
cargo run --release -p tools --bin extract_flagged_positions -- \
  runs/out_pass1.jsonl runs/p2_candidates.sfens --gap-threshold 35 --include-non-exact

# しきい値の別名（同義）: --max-gap-cp
cargo run --release -p tools --bin extract_flagged_positions -- \
  runs/out_pass1.jsonl - --max-gap-cp 20 | head

# STDIN/STDOUT による抽出
cat runs/out_pass1.jsonl | cargo run --release -p tools --bin extract_flagged_positions -- - - --gap-threshold 35

# JSONL の結合（SFEN重複は安定タイブレークで解消）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen

# 深さ優先（既定は EXACT 優先）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen --prefer-deeper

# モード指定（--prefer-deeper の明示的な別名）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen --mode depth-first

# STDIN/STDOUT を使った結合
cat runs/p1.jsonl | cargo run --release -p tools --bin merge_annotation_results -- \
  - - --dedup-by-sfen

# STDOUT へ出力しつつ manifest の出力先を明示
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl - --dedup-by-sfen --manifest-out runs/merge_manifest.json
```

## 学習ダッシュボード（最小）

軽量ベースライン（線形モデル）で、各エポックのメトリクスを CSV/PNG に出力し、CI でアーティファクト化できます。

### 使い方（baseline trainer）

```bash
# PNG も出す場合は features=plots でビルド
cargo build -p tools --features plots --release

# 最小ダッシュボード出力
./target/release/train_wdl_baseline \
  --input runs/train.jsonl --validation runs/valid.jsonl \
  --epochs 3 --batch-size 4096 --metrics --plots --seed 1 \
  --gate-val-loss-non-increase --gate-mode fail \
  --out runs/wdl_baseline
```

主なオプション:
- `--metrics`: 各エポックの CSV 出力を有効化
- `--plots`: 校正 PNG を出力（`tools` を `--features plots` でビルド時のみ有効）
- `--calibration-bins N`: 校正ビン数（既定 40）
- `--seed <u64>`: シャッフルの再現性シード（未指定時は非決定）
- `--gate-val-loss-non-increase`: 最終エポックが最良の `val_loss` でなければ FAIL/WARN
- `--gate-min-auc <f64>`: WDL 時の最小 AUC 閾値（既定無効）
- `--gate-mode {warn|fail}`: ゲートの動作

出力物（`runs/wdl_baseline_*` 配下）:
- `metrics.csv`（列）: `epoch, train_loss, val_loss, val_auc, val_ece, time_sec, train_weight_sum, val_weight_sum, is_best`
- `phase_metrics.csv`（列）: `epoch, phase, count, weighted_count, logloss, brier, accuracy, mae, mse`
  - WDL 時: `logloss, brier, accuracy` を出力、CP 時: `mae, mse` を出力、他は空欄
- `calibration_epoch_k.csv`: cp 等幅ビンごとの `mean_pred, mean_label(soft)`（±`--cp-clip` を等分）
- `calibration_epoch_k.png`: `mean_pred` と `mean_label(soft)` の 2 系列（X 軸は CP）
- `weights.json`: 最終エポックの重み
- `weights_best.json`: 最良 `val_loss` の重み（検証がある場合）

注意:
- 校正/ECE/phase 別メトリクスは **WDL + JSONL 検証** のときにのみ有効です（キャッシュ検証では cp/sfen が持てないためスキップ）。
- AUC は補助指標（既定ゲート OFF）。二値化しきい値は 0.5。
 - `val_ece` は **CP 等幅ビンに基づく ECE（cp-binned ECE）** です。一般的な確率ビン（0..1）の ECE とは異なります。

### 使い方（NNUE trainer）

```bash
# PNG も出す場合は features=plots でビルド
cargo build -p tools --features plots --release

# JSONL 入力（小規模サンプル）
./target/release/train_nnue \
  --input runs/train.jsonl --validation runs/valid.jsonl \
  --epochs 2 --batch-size 8192 --metrics --plots --seed 1 \
  --gate-val-loss-non-increase --gate-mode fail \
  --calibration-bins 40 \
  --out runs/nnue_dashboard

# キャッシュ入力（推奨）
./target/release/train_nnue \
  --input runs/train.cache --validation runs/valid.cache \
  --epochs 2 --batch-size 16384 --metrics --seed 1 \
  --out runs/nnue_cache

# ストリーミング（大規模キャッシュ向け）
./target/release/train_nnue \
  --input runs/train.cache \
  --epochs 1 --batch-size 16384 \
  --stream-cache --prefetch-batches 4 --throughput-interval 2.0 \
  --out runs/nnue_stream
```

主なオプション（train_nnue）:
- `--metrics`: 各エポックの CSV 出力を有効化
- `--plots`: 校正 PNG を出力（`tools` を `--features plots` でビルド時のみ有効）
- `--calibration-bins N`: 校正ビン数（既定 40）
- `--gate-val-loss-non-increase`: 最終エポックが最良の `val_loss` でなければ FAIL/WARN
- `--gate-min-auc <f64>`: WDL 時の最小 AUC 閾値（既定無効）
- `--gate-mode {warn|fail}`: ゲートの動作
- `--stream-cache`: キャッシュを逐次読み込み（学習前の全量読み込みを行わない）
- `--prefetch-batches N`, `--prefetch-bytes BYTES`, `--estimated-features-per-sample N`: プリフェッチ制御
- `--save-every N`: N バッチ毎にチェックポイントを保存（`checkpoint_batch_*.fp32.bin`）

出力物（`runs/nnue_*` 配下）:
- `metrics.csv`（列）: `epoch, train_loss, val_loss, val_auc, val_ece, time_sec, train_weight_sum, val_weight_sum, is_best`
- `phase_metrics.csv`（列）: `epoch, phase, count, weighted_count, logloss, brier, accuracy, mae, mse`（WDL+JSONL 検証のみ）
- `calibration_epoch_k.csv` / `calibration_epoch_k.png`（WDL+JSONL 検証のみ）
- `nn.fp32.bin`: 最終エポックの FP32 モデル
- `nn_best.fp32.bin`, `nn_best.meta.json`: 最良 `val_loss` のモデルとメタ情報（`best_epoch`, `best_val_loss`, 可能なら `best_val_auc`, `best_val_ece`）
- `nn.i8.bin`: 量子化（int8）版（`--quantized` 指定時）

注意:
- 校正/ECE/phase 別メトリクスは **WDL + JSONL 検証** のときにのみ有効です（キャッシュ検証では cp/sfen が持てないためスキップ）。
- AUC は補助指標（既定ゲート OFF）。二値化しきい値は 0.5。
- `val_ece` は **CP 等幅ビンに基づく ECE（cp-binned ECE）** です。一般的な確率ビン（0..1）の ECE とは異なります。

### CI アーティファクト（雛形）

`.github/workflows/train_dashboard.yml` を同梱。小さな JSONL を用意して 2 エポック実行、`runs/...` を artifact 化し、
`--gate-val-loss-non-increase --gate-mode fail` で回帰を赤化できます。


### ローカルでのクイック検証（サンプル生成→CSV/PNG確認）

最小サンプルの JSONL を作って、baseline/nnue のダッシュボード出力を手元で確認できます。

1) サンプル JSONL 作成
```bash
mkdir -p sample_data
cat > sample_data/train.jsonl << 'EOF'
{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":20,"seldepth":30,"bound1":"Exact","bound2":"Exact","best2_gap_cp":50}
{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1","eval":150,"depth":20,"seldepth":30,"bound1":"Exact","bound2":"Exact","best2_gap_cp":30}
EOF
cp sample_data/train.jsonl sample_data/val.jsonl
```

2) CSV だけ確認する場合（PNG不要）
```bash
cargo build -p tools --release

# Baseline
./target/release/train_wdl_baseline \
  --input sample_data/train.jsonl \
  --validation sample_data/val.jsonl \
  --epochs 2 --batch-size 64 \
  --metrics --calibration-bins 10 \
  --gate-val-loss-non-increase --gate-mode warn --seed 1 \
  --out runs/wdl_local

# NNUE
./target/release/train_nnue \
  --input sample_data/train.jsonl \
  --validation sample_data/val.jsonl \
  --epochs 2 --batch-size 128 \
  --metrics --calibration-bins 10 \
  --gate-val-loss-non-increase --gate-mode warn --seed 1 \
  --out runs/nnue_local

# 確認（例）
head -n 5 runs/wdl_local/metrics.csv
head -n 5 runs/nnue_local/metrics.csv
head -n 10 runs/nnue_local/calibration_epoch_1.csv
head -n 10 runs/nnue_local/phase_metrics.csv
```

3) PNG も出力する場合（Fontconfig が必要）
- Linux: `sudo apt-get install -y libfontconfig1-dev`
- macOS: `brew install fontconfig`
- Windows: WSL 推奨（もしくは CSV のみ利用）

```bash
cargo build -p tools --features plots --release

# --plots を付けて実行
./target/release/train_nnue \
  --input sample_data/train.jsonl \
  --validation sample_data/val.jsonl \
  --epochs 2 --batch-size 128 \
  --metrics --calibration-bins 10 --plots \
  --gate-val-loss-non-increase --gate-mode warn --seed 1 \
  --out runs/nnue_png

# 画像を開く
# macOS: open runs/nnue_png/calibration_epoch_1.png
# Linux: xdg-open runs/nnue_png/calibration_epoch_1.png
```

補足:
- 校正/ECE/phase 別メトリクスは **WDL + JSONL 検証** のときにのみ有効です。
- `val_ece` は **CP 等幅ビンに基づく ECE（cp-binned ECE）** です（確率ビンECEではありません）。


## Classic v1 蒸留・量子化

Classic v1 形式（`nn.classic.nnue`）への書き出しは、Single アーキで学習したネットを教師として
蒸留→量子化する二段ステップで行います。

### CLI の使い方

```bash
cargo run --release -p tools --bin train_nnue -- \
  --input runs/out.cache --arch classic --export-format classic-v1 \
  --distill-from-single runs/single_best.fp32.bin \
  --teacher-domain wdl-logit \
  --kd-loss bce --kd-temperature 2.0 --kd-alpha 0.8 \
  --out runs/classic_export
```

- `--arch classic --export-format classic-v1` を同時指定すると Classic 蒸留が有効になります。
- 教師ネット（Single FP32）のパスは `--distill-from-single` で必須指定です。
- `--teacher-domain cp|wdl-logit` で教師出力のスケール/意味空間を指定します。WDL ラベル + logit 教師の場合は `wdl-logit` を推奨。cp 評価を出す旧教師の場合は `cp` を指定してください（未指定時の自動推定: `label=cp` → `cp`, `label=wdl` → `wdl-logit`）。
- `--quant-ft` と `--quant-out` は `per-tensor` 固定です。Hidden 層 (`--quant-h1/-h2`) は 
  `per-tensor` / `per-channel` を切り替え可能です（既定: `per-channel`）。

### 量子化と整数パイプライン

- Feature Transformer (FT): i16 対称量子化 (per-tensor)。スケール `s_w0 = maxabs / 32767`。
- FT 出力は右シフト `CLASSIC_FT_SHIFT = 6`（除算 64）で i8 に落とし、`[-127,127]` へ飽和します。
- Hidden1 / Hidden2: 既定は per-channel i8 対称量子化（出力チャネル毎に maxabs を取得）。
- Output: 既定は per-tensor i8。Classic v1では per-channel 量子化はサポートされません。
- バイアスは `round_away_from_zero(b / (s_in * s_w))` で i32 化し、整数推論側と同一スケールになります。
- 書き出されたバイナリのレイアウトは
  `NNUE | version=1 | arch=HALFKP_256X2_32_32 | payload_len` のヘッダに続き、
  FT(i16)→FT bias(i32)→Hidden1(i8/i32)→Hidden2(i8/i32)→Output(i8/i32) 順で並びます。

### 蒸留ロスとハイパパラメータ

- `--kd-loss` は `mse` / `bce` / `kl` を選択可能（WDL）。教師ロジットは `--kd-temperature` で温度調整し、
  soft target を生成します。`--kd-alpha` は教師とデータラベルの線形合成比率です。
- CP ラベル時は `--kd-loss=mse` のみ有効です（`bce`/`kl` 指定時はエラー）。
- 損失値やスケール情報は構造化ログ (`phase: "distill_classic" / "classic_quantize"`) に JSON で出力します。
- 教師値ドメインと変換ロジックの詳細は `docs/distillation/teacher_value_domain.md` を参照してください。

### 注意事項

- Classic 蒸留は in-memory サンプルが必要です。`--stream-cache` でのストリーミング時はスキップされます。
- `--quant-ft=per-channel` は無効（エラー）です。FT の行列レイアウト上、per-tensor のみサポートします。


## バイナリ一覧（主要）

- データ生成/学習/解析
  - `generate_nnue_training_data`: 教師データ生成（JSONL/テキスト、レジューム/スキップ、MultiPV 対応）
  - `build_feature_cache`: JSONL→特徴キャッシュ（ストリーミング）
  - `train_nnue`: NNUE 学習（行疎更新、量子化保存対応）
  - `train_wdl_baseline` / `train_cp_baseline`: 軽量ベースライン学習（WDL/CP）
  - `analyze_teaching_quality`: 品質解析/要約/ゲート/複数入力比較/近似分位
    - `--dedup-by-sfen` の選定規則（固定）: `depth → seldepth → EXACT度 → nodes → time_ms`。完全同点では、先に現れた行（小さい `file_idx/line_idx`）を優先。
    - `--limit` と併用時は、上位（良い順）から取り込みます。完全同点のケースでは「先勝ち」のため、入力順（file/lineの小さい方）が優先されます。
  - `extract_flagged_positions`: JSONL から条件抽出
  - `merge_annotation_results`: JSONL マージと重複解消
  - `validate_cp_dataset`: CP テキストデータの整合チェック（レガシー）

- 定跡/ユーティリティ
  - `convert_opening_book`: 定跡 SFEN → バイナリ変換
  - `search_opening_book`: バイナリ定跡のハッシュ/局面検索
  - `sfen_hasher`: SFEN → ハッシュ値
  - `debug_position`: 局面デバッグ/パーフト/ムーブ適用
  - `create_mock_nnue`: テスト用 NNUE 重み生成

- ベンチマーク
  - `parallel_benchmark`: 並列探索ベンチ（NPS/重複/効率）
  - `nnue_benchmark`: 評価/探索のNNUE対比ベンチ
  - `simd_benchmark` / `simd_check`: SIMD の効果測定/機能確認
  - `quick_perf_test`: TT/探索の簡易パフォーマンステスト
  - `metrics_analyzer`: エンジンログ（`kind=bestmove_*`）の集計

### 量子化フォーマット（VERSION 3 概要）

`train_nnue --quantized` で保存される `nn.i8.bin` は以下の構造です。

- テキストヘッダ（行単位）
  - `NNUE` / `VERSION 3` / `FEATURES HALFKP` / `ACC_DIM <N>` / `RELU_CLIP <M>` / `FORMAT QUANTIZED_I8` / `END_HEADER`
- バイナリ本体（リトルエンディアン）
  1) `w0` の量子化パラメータ: `scale: f32` → `zero_point: i32`
  2) `w0` 本体: `i8` の配列（`input_dim * acc_dim` 要素）
  3) `b0` の量子化パラメータ: `scale: f32` → `zero_point: i32`
  4) `b0` 本体: `i8` の配列（`acc_dim` 要素）
  5) `w2` の量子化パラメータ: `scale: f32` → `zero_point: i32`
  6) `w2` 本体: `i8` の配列（`acc_dim` 要素）
  7) `b2` は `f32` のまま1要素

復元は `real ≈ (q - zero_point) * scale`（各重み配列で個別の `scale/zero_point` を使用）。
将来の互換性のため、フォーマット変更時は `VERSION` 行を更新します。

## 追加ドキュメント

- 教師データ生成ガイド: `docs/nnue-training-data-generation.md`
- 次タスク計画（本書）: `../../docs/tasks/nnue-training-pipeline-v3.md`

### ゲート設定サンプル
- サンプルファイル: `crates/tools/ci_gate.sample.json`
- 使用例:
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/final.jsonl --summary --gate crates/tools/ci_gate.sample.json --gate-mode fail
```

## 注意事項

- `build_feature_cache --compressor zst` を使用する場合は `tools` クレートを `--features zstd` でビルドしてください。
- `.gz` 入力は標準対応、`.zst` 入力は `--features zstd` 有効時に対応（`analyze_teaching_quality`/`merge_annotation_results`/`extract_flagged_positions`）。
- 旧テキスト系スクリプト・CP専用ワークフローはレガシー扱い（ベースライン検証用途に限り維持）。

### Manifest v2（生成ツール）
- `manifest_version: "2"` を採用。主なプロビナンス:
  - `teacher_engine { name, version, commit, usi_opts{hash_mb,multipv,threads,teacher_profile,min_depth} }`
    - `version` は原則として教師エンジンのバージョンを指します。環境変数 `ENGINE_SEMVER` が設定されていればそれを使用し、未設定の場合はジェネレータ（本ツール）の `CARGO_PKG_VERSION` を格納します。
    - `commit` は `ENGINE_COMMIT` が設定されていればそれを使用し、未設定の場合はビルド時に与えられた `GIT_COMMIT_HASH` を格納します。
  - `generation_command`, `seed`（`argv[1..]` を SHA-256 で安定生成）
  - `input { path, sha256, bytes }`, `nnue_weights_sha256`（重み使用時）
- 注意: `generation_command` は CLI 引数全体を含みます。機微情報は引数では渡さない運用を推奨します。
- `count` は全体の累計件数、`count_in_part` は当該 part のみの件数です（part出力時）。

### train_nnue の入力フィルタ（JSONL時）
- `--exclude-no-legal-move`: 合法手なしの局面を除外
- `--exclude-fallback`: 探索でフォールバックが発生した局面を除外
- これらはキャッシュビルダー（build_feature_cache）の `--exclude-*` と同等です。キャッシュ入力ではビルド時のフィルタ結果が反映されます。

### merge_annotation_results の仕様補足
- 入力圧縮: `.jsonl`, `.jsonl.gz`（標準）、`.jsonl.zst`（`--features zstd`）。
- 非 dedup: 入力をそのまま連結し、順序を保持（JSON の最小検証あり）。
- dedup の選定規則（既定 = EXACT 優先）:
  - EXACT 優先: `EXACT度 → depth → seldepth → nodes → time_ms → file_idx → line_idx`
  - 深さ優先（`--prefer-deeper`）: `depth → seldepth → EXACT度 → nodes → time_ms → file_idx → line_idx`
  - 完全同点では、先に現れた行（小さい `file_idx/line_idx`）を保持。
  - 注: `nodes` / `time_ms` は「大きいほど優先」します（より計算資源を掛けた結果を採用）。
- 出力順の安定化: dedup 時の最終出力は `sfen` 昇順で書き出し。
- STDIN/STDOUT: 入力に `-` を指定すると STDIN を読む。出力に `-` を指定すると STDOUT に書き出し。
- マニフェスト統合: 既定では出力ファイルと同ディレクトリに `manifest.json` を生成。出力が STDOUT の場合は
  `--manifest-out <path>` で出力先を明示可能（未指定時は警告を出しスキップ）。
- マニフェスト内容: `mode/inputs/sources` に加え、行カウントの詳細（`read_lines`/`valid_json_lines`/`written_lines`、互換のため `total_positions` も維持）、
  統計（min/max/avg: depth/seldepth/nodes/time_ms）、設定整合性（multipv/teacher_profile/hash_mb の一致 or varies）、
  `generated_at_range` は ISO-8601 を安全に比較（タイムゾーン付き文字列は UTC に正規化、パース不能時は文字列比較にフォールバック）。

### extract_flagged_positions の仕様補足
- 入出力: 入力に `-` を指定すると STDIN を読む。出力未指定または `-` 指定で STDOUT に書き出し。
- 入力圧縮: `.jsonl`, `.jsonl.gz`（標準）、`.jsonl.zst`（`--features zstd`）。

### cross‑dedup（train/valid/test の漏洩チェック）
- ツール: `check_cross_dedup`
- 使い方:
  ```bash
  cargo run -p tools --bin check_cross_dedup -- \
    --train runs/train.jsonl --valid runs/valid.jsonl --test runs/test.jsonl \
    --report runs/leak_report.csv
  ```
- 重複（SFEN key の一致）が見つかると `leak_report.csv` に出力し、非ゼロ終了（CI で赤）。
- オプション: `--include-intra` を指定すると、同一split内の重複も検出対象に加えます（既定はcrossのみ）。
- 正規化: SFENは「最初の4トークン（board, side, hands, move count）」に正規化して比較します。

### JSONL出力の補足
- `lines_origin`: `k2` または `k3` を記録（K=3再探索で採用したかの可観測性）。

## 曖昧掘りオーケストレーション（抽出→再注釈→マージ）

1コマンドで pass1 の結果から曖昧候補を抽出し、強設定で再注釈（K=3/entropy等）して最終マージまでを行います。マージは常に `--mode depth-first` を明示し、再現性を担保します。中間ファイルと系譜は orchestration manifest に記録されます。

```bash
# 例: pass1(out_pass1.jsonl)から曖昧抽出→再注釈→マージ
cargo run --release -p tools --bin orchestrate_ambiguous -- \
  --pass1 runs/out_pass1.jsonl \
  --final runs/final.jsonl \
  --gap-threshold 35 \
  --engine enhanced --multipv 3 --hash-mb 64 \
  --split 200000 --compress gz

# ドライラン（実行計画のみ表示）
cargo run --release -p tools --bin orchestrate_ambiguous -- \
  --pass1 runs/out_pass1.jsonl --final runs/final.jsonl --dry-run
```

ドライラン出力は、空白や二重引用符を含むパスも適切に引用されており、そのままコピペ実行できます（PowerShell / cmd いずれでも動作）。

出力例（パスは例示、実環境に合わせて変化します）:
```text
[dry-run] "/path/to/target/debug/extract_flagged_positions" "runs/out_pass1.jsonl" - --gap-threshold 35
[dry-run] normalize+unique (in-mem) -> ".final.ambdig/pass2_input.sfens"
[dry-run] "/path/to/target/debug/generate_nnue_training_data" ".final.ambdig/pass2_input.sfens" ".final.ambdig/pass2.jsonl" --engine enhanced --output-format jsonl --hash-mb 64 --multipv 3 --teacher-profile balanced --split 200000 --compress gz
[dry-run] "/path/to/target/debug/merge_annotation_results" --dedup-by-sfen --mode depth-first --manifest-out "runs/final.manifest.json" "runs/out_pass1.jsonl" ".final.ambdig/pass2.jsonl" "runs/final.jsonl"
[dry-run] "/path/to/target/debug/analyze_teaching_quality" "runs/final.jsonl" --json --expected-multipv 3 --manifest-autoload-mode strict > ".final.ambdig/quality.json"
[dry-run] would write orchestration manifest to ".final.ambdig/orchestrate_ambiguous.manifest.json"
```

備考:
- Windows の場合も、空白や `"` を含むパスは適切に引用されます（`"` は二重化してから全体を `"..."` で囲みます）。

主なオプション:
- 抽出: `--gap-threshold <cp>`、`--include-non-exact`、`--include-aspiration-failures <N>`、`--include-mate-boundary`
- 再注釈(generate 委譲): `--engine`、`--nnue-weights`、`--teacher-profile`、`--multipv`、`--min-depth`、`--nodes|--time-limit-ms`、`--jobs`、`--hash-mb`、`--reuse-tt`、`--split`、`--compress`
- 曖昧/entropy: `--amb-gap2-threshold`、`--amb-allow-inexact`、`--entropy-mate-mode`、`--entropy-scale`
- マージ: `--merge-mode depth-first`（常に明示）
- 正規化: `--normalize-sort-unique`（外部ソート＋uniq） / `--normalize-chunk-lines N` / `--normalize-merge-fan-in K`
- 要約: `--analyze-summary`（JSONを `<out-dir>/quality.json` に保存）
- 削除: `--prune`（常に削除）/ `--prune-on-success`（成功時のみ削除）
- 実行制御: `--dry-run` / `--verbose` / `--keep-intermediate`（既定ON） / `--prune`（常に中間物削除） / `--prune-on-success`（成功時のみ削除）
 - 正規化: `--normalize-sort-unique`（外部ソート＋uniqで省メモリ化）/ `--normalize-chunk-lines <N>`（既定 200k 行）/ `--normalize-merge-fan-in <N>`（多段マージの同時オープン上限、既定 256）

出力:
- `<final>.manifest.json`: マージ結果の aggregated manifest
- `<out-dir>/orchestrate_ambiguous.manifest.json`: オーケストレーション全体の系譜と整合サマリ（`counts` に `pass1_total` / `extracted` / `pass2_generated` / `final_written` を記録。`pass1_total_by_source` も付与）

補足:
- マージモードは常に `--mode depth-first` を明示（既定の exact-first と混同しない）。
- `--analyze-summary` は pass2 の `multipv` を検知して `--expected-multipv` を自動設定します（検出不可時は CLI の `--multipv` を使用）。
- 詳細設計は `docs/tasks/orchestrate_ambiguous_plan.md` を参照。

### 閾値の使い分け（gap と gap2）
- 抽出 `--gap-threshold`: 再注釈候補を広く拾うための“粗い”しきい値（例: 30–50cp）。
- 再注釈 `--amb-gap2-threshold`: K=3 の曖昧度（1位と2位の差）を厳密に測る“細かい”しきい値（例: 15–35cp）。
- ガイド: `analyze_teaching_quality --summary` の統計（中央値/下位5%）を参考に現場の負荷と精度で調整。

### out-dir の構成（例）
`.<final-stem>.ambdig/` 配下:
- `pass2_input.tmp` / `pass2_input.sfens`
- `pass2.jsonl` または `pass2.part-*.jsonl.{gz|zst}`（+ 各 manifest）
- `quality.json`（`--analyze-summary` 時）
- `orchestrate_ambiguous.manifest.json`

複数 `--pass1` を与えた場合のマージ優先は、depth-first + dedup により「`pass1` を与えた順 → `pass2`」です。同一 SFEN の採用元に影響するため、順序は固定して運用してください。

メモリ対策:
- 大規模抽出時は `--normalize-sort-unique` を指定すると、`pass2_input.tmp` をチャンクに分割してソート＆uniqし、k-wayマージで `pass2_input.sfens` を生成します。メモリ使用を抑えつつ重複除去が可能です（I/O は増加）。FD上限に配慮し、多段マージ（`--normalize-merge-fan-in`）で安全に処理します。

Prune 補足:
- `--dry-run --prune` / `--dry-run --prune-on-success` では、削除計画（対象件数・合計サイズ）を表示します（`--verbose` で対象ファイル一覧も表示）。
- 中間 manifest は prune 対象です（`pass2.manifest.json` と各 `pass2.part-*.manifest.json` を含む）。集約情報は orchestrator の manifest に記録されます。
