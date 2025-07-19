# WASM API クイックリファレンス

## 基本的な使い方

### 1. エンジン初期化
```typescript
const engine = new WasmEngine();
await engine.sendCommand('usi');
await engine.sendCommand('isready');
```

### 2. 局面設定
```typescript
// 初期局面
await engine.sendCommand('position startpos');

// SFEN指定
await engine.sendCommand('position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1');

// 手順付き
await engine.sendCommand('position startpos moves 7g7f 3c3d');
```

### 3. 探索実行
```typescript
// 深さ指定
await engine.sendCommand('go depth 10');

// 時間指定（ミリ秒）
await engine.sendCommand('go movetime 1000');

// ノード数指定
await engine.sendCommand('go nodes 1000000');
```

### 4. 結果取得
```typescript
// ポーリングで結果を待つ
const interval = setInterval(() => {
    const result = engine.get_search_result();
    if (result) {
        clearInterval(interval);
        console.log(result); // "bestmove 7g7f"
    }
}, 100);
```

### 5. 探索停止
```typescript
await engine.sendCommand('stop');
```

## USIコマンド一覧

| コマンド | 説明 | 例 |
|---------|------|-----|
| `usi` | エンジン情報取得 | `usi` |
| `isready` | 初期化確認 | `isready` |
| `setoption` | オプション設定 | `setoption name Hash value 256` |
| `position` | 局面設定 | `position startpos moves 7g7f` |
| `go` | 探索開始 | `go depth 15 movetime 3000` |
| `stop` | 探索停止 | `stop` |

## goコマンドパラメータ

| パラメータ | 説明 | 例 |
|-----------|------|-----|
| `depth` | 探索深さ | `go depth 20` |
| `movetime` | 思考時間（ミリ秒） | `go movetime 5000` |
| `nodes` | 探索ノード数 | `go nodes 10000000` |
| `infinite` | 無限探索 | `go infinite` |

## 応答フォーマット

### エンジン情報
```
id name ShogiEngine WASM 1.0
id author YourName
usiok
```

### 準備完了
```
readyok
```

### 探索情報
```
info depth 10 seldepth 15 nodes 1234567 nps 2000000 score cp 150 pv 7g7f 3c3d 2g2f
```

### 最善手
```
bestmove 7g7f ponder 3c3d
```

## エラーハンドリング

```typescript
try {
    const response = await engine.sendCommand('invalid command');
    if (response.startsWith('error')) {
        console.error('Command error:', response);
    }
} catch (e) {
    console.error('Engine error:', e);
}
```