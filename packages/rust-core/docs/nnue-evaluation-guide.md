# NNUE評価ガイド

このドキュメントでは、NNUEモデルの性能評価方法について説明します。

## 概要

NNUEモデルの品質を確保するため、新しいモデルと既存のベースラインモデルを比較評価する仕組みを提供しています。

## 評価方法

### 1. ローカル評価スクリプト（推奨）

`scripts/nnue/evaluate-nnue.sh`を使用してローカルで詳細な評価を実行できます。

#### 基本的な使用方法

```bash
cd packages/rust-core
./scripts/nnue/evaluate-nnue.sh [baseline.nnue] [candidate.nnue] [games] [threads]
```

#### パラメータ

- `baseline.nnue`: 比較基準となる既存のNNUEファイル（デフォルト: `runs/nnue_local/baseline.nnue`）
- `candidate.nnue`: 評価対象の新しいNNUEファイル（デフォルト: `runs/nnue_local/candidate.nnue`）
- `games`: 対戦ゲーム数（デフォルト: 1000）
- `threads`: 並列実行スレッド数（デフォルト: 8）

#### 実行例

```bash
# デフォルト設定で実行
./scripts/nnue/evaluate-nnue.sh

# カスタム設定で実行
./scripts/nnue/evaluate-nnue.sh baseline.nnue new_model.nnue 2000 16

# 新しくトレーニングしたモデルを評価
cp runs/train_nnue_*/final_weights.nnue candidate.nnue
./scripts/nnue/evaluate-nnue.sh runs/ref.nnue candidate.nnue
```

#### 評価結果

スクリプトは以下の情報を出力します：

- **勝率**: candidateがbaselineに対してどれだけ勝ったか
- **NPS (Nodes Per Second)**: 探索速度の比較
- **Gate判定**: 
  - **Pass**: 55%以上の勝率かつNPS差が±3%以内（採用推奨）
  - **Provisional**: 統計的に有意だが、Pass条件は未達
  - **Reject**: 明確に劣っている（採用非推奨）

### 2. gauntletツールの直接使用

より詳細な制御が必要な場合は、`gauntlet`ツールを直接使用できます：

```bash
# ビルド
cargo build -p tools --release --bin gauntlet --features nnue_telemetry

# 実行
target/release/gauntlet \
  --base baseline.nnue \
  --cand candidate.nnue \
  --time "0/10+0.1" \
  --games 1000 \
  --threads 8 \
  --hash-mb 1024 \
  --book docs/reports/fixtures/opening/representative_100.epd \
  --json result.json \
  --report report.md
```

### 3. CI/CDでの軽量チェック

`.github/workflows/gauntlet-regression-check.yml`により、コード変更時に自動的に軽量なリグレッションチェックが実行されます。これは重大な性能劣化の検出のみを目的としています。

## ベストプラクティス

1. **新しいNNUEモデルの評価**
   - 最低1000ゲーム以上で評価
   - 複数スレッドで並列実行して時間短縮
   - 異なる開局から開始して偏りを防ぐ

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
