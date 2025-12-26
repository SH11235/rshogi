## Analyze Bundle

Webアプリのバンドルサイズを分析し、削減ポイントを特定するコマンドです。

```
/analyze-bundle [--visual]
```

### オプション

- `--visual`: ビジュアルレポート（stats.html）を生成して開く

### 実行内容

1. **Webアプリをビルド**してバンドルサイズを確認
2. **依存関係ごとのサイズ内訳**を分析
3. **削減候補**を特定して報告

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

### 削減施策の例

| 施策 | 効果 | 難易度 |
|------|------|--------|
| wasm-opt 適用 | WASM -35% | 低 |
| コード分割 (lazy load) | 初期ロード削減 | 中 |
| 未使用コンポーネント削除 | -10~50KB | 低 |
| tailwind-merge → clsx | -90KB ※非推奨 | - |

### 目標サイズ（gzip後）

| カテゴリ | 目標 |
|----------|------|
| JS (メイン) | < 150KB |
| WASM | < 200KB |
| CSS | < 10KB |
| **合計** | **< 360KB** |
