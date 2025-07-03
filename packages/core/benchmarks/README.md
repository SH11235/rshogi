# AI Benchmarks

このディレクトリにはAIエンジンのパフォーマンスベンチマークツールが含まれています。

## ベンチマークの実行

```bash
# AIベンチマークを実行
npm run benchmark:ai

# 詰将棋ベンチマークを実行
npm run test benchmarks/mate-search.benchmark.test.ts
```

## ベンチマーク結果の表示

### 方法1: コマンドラインから開く
```bash
npm run benchmark:viewer
```

### 方法2: ローカルサーバーで開く（WSL推奨）
```bash
npm run benchmark:serve
```
自動的にhttp-serverが起動し、ブラウザでviewer.htmlが開きます。

### 方法3: 手動で開く
`benchmarks/viewer.html` をブラウザで開いてください。

### WSL環境での使用
WSL環境では、以下のいずれかの方法で開けます：

1. **npm scriptを使用**（自動的にWindows側のブラウザで開きます）
   ```bash
   npm run benchmark:viewer
   ```

2. **手動でWindowsのパスに変換**
   ```bash
   # WSLパスをWindowsパスに変換して開く
   explorer.exe $(wslpath -w packages/core/benchmarks/viewer.html)
   ```

3. **Node.jsの簡易サーバーを使用**
   ```bash
   # benchmarksディレクトリに移動
   cd packages/core/benchmarks
   # npxでローカルサーバーを起動
   npx http-server -p 8000
   # ブラウザでhttp://localhost:8000/viewer.htmlにアクセス
   ```

4. **VS CodeのLive Server拡張機能を使用**
   - VS Codeで「Live Server」拡張機能をインストール
   - `viewer.html`を右クリック → 「Open with Live Server」を選択

### 使い方
1. ベンチマーク結果のJSONファイルを選択するか、ドラッグ&ドロップ
2. インタラクティブなグラフで結果を分析
   - 平均思考時間
   - 探索深度
   - 探索ノード数
   - 局面別パフォーマンス

## ベンチマーク結果ファイル

- `ai-benchmark-*.json` - AIベンチマークの詳細結果
- `mate-search-*.benchmark.json` - 詰将棋ベンチマークの結果
- `benchmark-results.md` - AIベンチマークの要約（Markdown形式）

## ベンチマークの内容

### AIベンチマーク
- 初期局面、中盤、終盤の3つの局面でテスト
- 4つの難易度レベル（beginner, intermediate, advanced, expert）
- 各局面での思考時間、探索深度、ノード数を測定

### 詰将棋ベンチマーク
- 1手詰め、3手詰め、5手詰めの問題
- 詰み探索の正確性と速度を測定