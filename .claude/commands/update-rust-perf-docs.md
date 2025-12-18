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
   - NNUE有効時のperf結果（`*_nnue_*.txt`）
   - Material評価時のperf結果（`*_release.txt`）
   - NNUE有効時のbenchmark結果（`nnue_enabled: true`）
   - Material評価時のbenchmark結果（`nnue_enabled: false`）

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
- CPU%の値は小数点2桁まで記載
- 関数名が変わっている場合は適切に更新
