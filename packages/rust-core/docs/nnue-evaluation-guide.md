# NNUE評価ガイド

このドキュメントでは、NNUEモデルの性能評価方法について説明します。

## 概要

NNUEモデルの品質を確保するため、新しいモデルと既存のベースラインモデルを比較評価する仕組みを提供しています。

## 評価方法

### 1. ローカル評価スクリプト（推奨 / Spec 013 準拠）

`scripts/nnue/evaluate-nnue.sh`を使用してローカルで詳細な評価を実行できます。Spec 013 により評価時の `--threads` は 1 で固定です（スクリプト側で強制）。Opening book は既定で `runs/fixed/20251011/openings_ply1_20_v1.sfen` を使用します（環境変数 `EVAL_BOOK` で差し替え可能）。PV spread は `--pv-ms` により探索時間を延ばせますが、内部条件によりサンプル 0 となる場合があるため、補助指標は `pv_probe` を用いて別途採取します。

#### 基本的な使用方法

```bash
cd packages/rust-core
./scripts/nnue/evaluate-nnue.sh [baseline.nnue] [candidate.nnue] [games] [threads=1] [pv_ms=500]
```

#### パラメータ

- `baseline.nnue`: 比較基準となる既存のNNUEファイル（デフォルト: `runs/nnue_local/baseline.nnue`）
- `candidate.nnue`: 評価対象の新しいNNUEファイル（デフォルト: `runs/nnue_local/candidate.nnue`）
- `games`: 対戦ゲーム数（デフォルト: 1000）
- `threads`: 評価スレッド数。Spec 013 により 1 固定（引数で与えても内部で 1 に強制）
- `pv_ms`: PV 計測時間（ミリ秒）。既定 500ms、必要に応じて 1000〜3000ms に増やすとサンプルが得やすい

#### 実行例

```bash
# デフォルト設定で実行
./scripts/nnue/evaluate-nnue.sh

# カスタム設定で実行（threads は 1 を指定。内部でも 1 に強制）
EVAL_BOOK=runs/fixed/20251011/openings_ply1_20_v1.sfen \
  ./scripts/nnue/evaluate-nnue.sh baseline.nnue new_model.nnue 2000 1 1000

# 新しくトレーニングしたモデルを評価
cp runs/train_nnue_*/final_weights.nnue candidate.nnue
./scripts/nnue/evaluate-nnue.sh runs/ref.nnue candidate.nnue
```

#### 評価結果

スクリプトは以下の情報を出力します：

- **勝率**: candidateがbaselineに対してどれだけ勝ったか
- **NPS (Nodes Per Second)**: 探索速度の比較
- **Gate判定**（本プロジェクト運用）: 
  - **Pass**: 勝率≥55% または Elo +6（95%CI下限>0）かつ |ΔNPS|≤3%（強化幅が大きい場合は+5%まで許容）
  - **Provisional**: 統計的に有意だが、Pass条件は未達（追試推奨）
  - **Reject**: 95%CIの下限≤0 もしくは明確に劣る
  - 備考: gauntlet 内部の PV 計測はサンプル 0 となる場合があり、採否は勝率/Elo/NPS で決定。PV spread は `pv_probe` 結果を補助として保存。

### 2. gauntletツールの直接使用

より詳細な制御が必要な場合は、`gauntlet`ツールを直接使用できます：

```bash
# ビルド
cargo build -p tools --release --bin gauntlet --features nnue_telemetry

# 実行（threads=1 固定）
target/release/gauntlet \
  --base baseline.nnue \
  --cand candidate.nnue \
  --time "0/10+0.1" \
  --games 1000 \
  --threads 1 \
  --hash-mb 1024 \
  --book runs/fixed/20251011/openings_ply1_20_v1.sfen \
  --json result.json \
  --report report.md

### 3. 量子化の影響切り分け（Classic）

Classic の量子化後モデル（INT）が長TCで伸びない場合、まず FP32 で長TCを回し、次に FP32 と INT の出力差を測定して切り分けます。

1) 非量子化 Classic（FP32）の強度確認（注意）
  - 多くのエンジンは Classic のランタイムで INT8 を前提にしており、FP32 Classic を直接対局で評価できない場合があります。本リポジトリの `gauntlet` も INT8 Classic と Single に対応しています。
  - そのため、FP32 Classic の強度確認は対局ではなく 2) のラウンドトリップ誤差（FP32 対 INT）の測定で代替します。

2) FP32 vs INT のラウンドトリップ誤差（verify_classic_roundtrip）
```bash
head -n 5000 runs/fixed/20251011/openings_ply1_20_v1.sfen > runs/tmp/rt_suite_5k.sfen
cargo run -p tools --release --bin verify_classic_roundtrip -- \
  --fp32 runs/phase1_.../classic_v1/nn_best.fp32.bin \
  --int  runs/phase1_.../classic_v1_q/nn.classic.nnue \
  --positions runs/tmp/rt_suite_5k.sfen \
  --out rt_diff_5k.json --worst-jsonl rt_worst_5k.jsonl --worst-count 50
```

3) 対応の指針
- FP32が強く、INTが弱い → 量子化校正の見直し（校正サンプル増量、`--quant-search` 継続、`relu_clip`/per-channel指定の見直しなど）。
- FP32自体が弱い → データ/学習の再強化（TIME_MS↑、ユニーク↑、追加エポック、再蒸留）。

### 4. PV spread の取得（`pv_probe` 推奨）

gauntlet の内部計測は条件により `pv_spread_samples=0` となることがあるため、PV spread は `pv_probe` を用いて別途採取します。

```bash
cargo build -p tools --release --bin pv_probe
target/release/pv_probe \
  --cand candidate.nnue \
  --book runs/fixed/20251011/openings_ply1_20_v1.sfen \
  --depth 6 --threads 1 --hash-mb 512 \
  --samples 100 --seed 42 \
  --json pv_probe_d6_s100.json
```
```

### 5. CI/CDでの軽量チェック

`.github/workflows/gauntlet-regression-check.yml`により、コード変更時に自動的に軽量なリグレッションチェックが実行されます。これは重大な性能劣化の検出のみを目的としています。

## ベストプラクティス

1. **新しいNNUEモデルの評価**
   - 最低1000ゲーム以上で評価
   - threads=1 固定（Spec 013）。所要時間短縮はゲーム数/TCを調整
   - 異なる開局から開始して偏りを防ぐ（固定スイートを使用）

2. **評価環境**
   - CPUリソースが豊富なローカル環境を推奨
   - 評価中は他の重いプロセスを避ける
   - 同一マシンで比較して公平性を保つ

3. **結果の解釈**
   - 単一の評価結果に依存しない
   - 必要に応じて複数回評価を実施
   - Gate判定は参考指標として活用

## トラブルシューティング

### エラー: "Opening book not found"

開局データベースファイルが必要です：
```bash
ls docs/reports/fixtures/opening/representative_100.epd
```

### エラー: "Insufficient memory"

hash-mbパラメータを調整：
```bash
./scripts/nnue/evaluate-nnue.sh
# スクリプトを編集して--hash-mbを512などに変更
```

## 関連ドキュメント

- [NNUE Training Guide](./training-guide.md) - NNUEモデルのトレーニング方法
- [Classic Teacher Adoption Plan](./plans/classic_teacher_adoption_plan.md) - Classic NNUE実装計画
