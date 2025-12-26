## Analyze Bundle

Webアプリのバンドルサイズを分析し、削減ポイントを特定するコマンドです。

```
/analyze-bundle [--visual]
```

### オプション

- `--visual`: ビジュアルレポート（stats.html）を生成して開く

### 実行内容

1. **過去の分析記録を確認**: `docs/bundle-analysis.md` を読む
2. **Webアプリをビルド**してバンドルサイズを確認
3. **依存関係ごとのサイズ内訳**を分析
4. **削減候補**を特定して報告
5. **分析結果を記録**: `docs/bundle-analysis.md` を更新

### 手動実行

```bash
# 通常ビルド（サイズ確認のみ）
pnpm --filter web build

# ビジュアル分析レポート生成
pnpm --filter web build:analyze
# → dist/stats.html が生成され、ブラウザで開く
```

### 分析観点

1. **node_modules の大きなパッケージ**
   - react-dom, tailwind-merge, @radix-ui 等

2. **packages/* の内訳**
   - 自作コード（ui, app-core等）のサイズ

3. **WASM サイズ**
   - engine_wasm_bg.wasm のサイズと最適化状況

### 分析記録

過去の分析結果・判断は `docs/bundle-analysis.md` に記録されています。
新しい施策を検討する前に、過去に検討済みの施策を確認してください。

### 目標サイズ（gzip後）

| カテゴリ | 目標 |
|----------|------|
| JS (メイン) | < 150KB |
| WASM | < 200KB |
| CSS | < 10KB |
| **合計** | **< 360KB** |
