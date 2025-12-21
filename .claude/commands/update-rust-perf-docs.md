# Rust エンジン パフォーマンスドキュメント更新

`packages/rust-core/docs/performance/README.md` を最新の計測結果で更新します。

## 前提条件

以下のスクリプトを実行済みであること:

```bash
cd packages/rust-core
./scripts/perf_all.sh
```

このスクリプトは内部でsudoを使用するため、ユーザーが実行する必要があります。

## 手順

1. **計測結果の確認**
   - `packages/rust-core/perf_results/` ディレクトリの最新ファイルを確認
   - `packages/rust-core/benchmark_results/` ディレクトリの最新ファイルを確認

2. **結果の読み取り**

   **perfレポート（推奨: フラットレポートを優先）**
   - `nnue_flat.txt` - NNUE有効時のフラットレポート（--no-children、自己時間のみ）**← 最も正確**
   - `nnue_callers.txt` - NNUE有効時のコールグラフ（-g caller、呼び出し元情報付き）
   - `*_nnue_release.txt` - NNUE有効時の詳細レポート（コールツリー付き）
   - `*_release.txt` - Material評価時のレポート

   **ベンチマーク結果**
   - NNUE有効時のbenchmark結果（`nnue_enabled: true`）
   - Material評価時のbenchmark結果（`nnue_enabled: false`）

   **フラットレポート vs 詳細レポートの違い**
   - `nnue_flat.txt`: 各関数の自己時間（self time）のみ。ホットスポット一覧の更新に最適
   - `*_nnue_release.txt`: コールツリー付き。関数の内訳分析に有用

3. **ドキュメント更新**
   - `packages/rust-core/docs/performance/README.md` のホットスポット一覧を更新
   - 計測日を更新
   - 変更履歴に追記

## 実行

最新の計測結果ファイルを読み込み、`packages/rust-core/docs/performance/README.md` を更新してください。

主な更新項目:
- 「ホットスポット一覧」セクションのCPU%
- 計測環境の「計測日」
- 「変更履歴」に新しいエントリを追加

注意:
- 調査完了項目（MovePicker等）の内容は変更しないこと
  - `### MovePicker (調査完了)` セクション内の計測値（CPU%等）は当時の調査時の値のまま残し、更新しない
- CPU%の値は小数点2桁まで記載
- 関数名が変わっている場合は適切に更新
