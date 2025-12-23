# Rust パフォーマンスドキュメント更新のレビュー

`packages/rust-core/scripts/perf_all.sh` を実行し、パフォーマンスドキュメントを更新しました。

## 確認してほしいこと

1. **計測結果ファイル**
   - `packages/rust-core/perf_results/` に最新のperfレポートが出力されています
   - `packages/rust-core/benchmark_results/` に最新のベンチマーク結果（JSON）が出力されています

2. **ドキュメント更新**
   - `packages/rust-core/docs/performance/README.md` を更新しました

## レビュー依頼

以下の観点で変更が意図通りか確認してください：

- NPS計測結果（NNUE/Material両方）が正しく更新されているか
- ホットスポット一覧のCPU%が最新の計測値を反映しているか
- 変更履歴に適切なエントリが追加されているか
- 前回計測との比較分析が妥当か（改善点、相対変動、順位変動など）

以下のいずれかの方法で変更内容を確認し、問題があれば指摘してください：

- 未コミットの場合: `git diff` で変更を確認
- コミット済みの場合: `git log -1 -p` で直近のコミット内容を確認
- mainブランチとの差分: `git diff main` または `git log main..HEAD --oneline` で改修内容全体を把握
