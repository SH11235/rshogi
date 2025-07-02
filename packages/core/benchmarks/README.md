# Mate Search Benchmarks

このディレクトリには詰み探索のパフォーマンスベンチマーク結果が保存されます。

## 使い方

```bash
npm run benchmark
```

## ファイル形式

- `mate-search-javascript-*.benchmark.json` - JavaScript実装のベンチマーク結果
- `mate-search-rust-wasm-*.benchmark.json` - Rust/WASM実装のベンチマーク結果（今後追加予定）

## ベンチマーク内容

6つの標準的な詰将棋問題を使用：
- 1手詰め（2問）
- 3手詰め（2問）
- 5手詰め（1問）
- 詰みなし（1問）

## 基準値（JavaScript実装）

- 平均NPS: 300-600
- 総ノード数: 約20-30（深さ7まで）

## 注意事項

- コード変更による性能の変化を追跡するため、安定したバージョンのベンチマーク結果のみをGitにコミットしてください
- 一時的なベンチマーク結果は `.gitignore` により除外されます