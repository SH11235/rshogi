# Rust将棋エンジン WASM統合計画

## 概要
rust-coreの将棋AIエンジンをWebアプリケーションから利用可能にするため、WASM APIを設計・実装する。USIプロトコルのサブセットをベースに、TypeScriptから使いやすいインターフェースを提供する。

## 現状分析

### 既存実装の構成
1. **Rust側（rust-core）**
   - `Engine` - 探索エンジンのメインクラス
   - `SearchLimits` - 探索制限（深さ、時間、ノード数）
   - `SearchResult` - 探索結果（最善手、評価値、統計）
   - NNUEとMaterial評価関数の選択が可能

2. **TypeScript側（web）**
   - `AIService` - WebWorkerベースのAIインターフェース
   - `calculateMove()` - 指し手計算
   - `evaluatePosition()` - 局面評価
   - 難易度設定、思考時間管理

### 新ディレクトリ構成（予定）
```
shogi-engine/
├── crates/
│   ├── engine-core/    # コアライブラリ
│   ├── engine-cli/     # USI CLIアダプタ
│   └── engine-wasm/    # WASM APIアダプタ（新規）
```

## WASM API設計

### 1. USIプロトコルサブセット
Webブラウザ環境に適したUSIコマンドのサブセットを実装：

#### 必須コマンド
- `usi` - エンジン識別
- `isready` / `readyok` - 初期化確認
- `setoption` - オプション設定
- `position` - 局面設定
- `go` - 探索開始
- `stop` - 探索停止
- `quit` - 終了

#### 省略するコマンド
- `usinewgame` - Web環境では不要
- `ponderhit` - 先読み機能は初期実装では対応しない
- `gameover` - Web側で管理

### 2. WASM公開インターフェース

```rust
// engine-wasm/src/lib.rs

#[wasm_bindgen]
pub struct WasmEngine {
    engine: Engine,
    position: Position,
    search_handle: Option<SearchHandle>,
}

#[wasm_bindgen]
impl WasmEngine {
    /// エンジンを作成
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmEngine, JsValue> {
        // NNUEエンジンで初期化
        Ok(WasmEngine {
            engine: Engine::new(EngineType::Nnue),
            position: Position::startpos(),
            search_handle: None,
        })
    }

    /// USIコマンド処理
    #[wasm_bindgen]
    pub fn process_command(&mut self, command: &str) -> String {
        match self.parse_usi_command(command) {
            UsiCommand::Usi => self.handle_usi(),
            UsiCommand::IsReady => self.handle_isready(),
            UsiCommand::SetOption { name, value } => self.handle_setoption(name, value),
            UsiCommand::Position { sfen, moves } => self.handle_position(sfen, moves),
            UsiCommand::Go { params } => self.handle_go(params),
            UsiCommand::Stop => self.handle_stop(),
            _ => "error Unknown command".to_string(),
        }
    }

    /// 非同期探索結果の取得
    #[wasm_bindgen]
    pub fn get_search_result(&mut self) -> Option<String> {
        // 探索完了していれば結果を返す
        if let Some(handle) = &self.search_handle {
            if handle.is_finished() {
                let result = handle.get_result();
                self.search_handle = None;
                return Some(self.format_bestmove(result));
            }
        }
        None
    }

    /// 現在の評価値を取得
    #[wasm_bindgen]
    pub fn evaluate_current_position(&self) -> i32 {
        self.engine.evaluate(&self.position)
    }
}
```

### 3. TypeScript側インターフェース

```typescript
// packages/web/src/services/ai/wasmEngineService.ts

export interface WasmEngineService {
    // エンジン初期化
    initialize(): Promise<void>;
    
    // USIコマンド送信
    sendCommand(command: string): Promise<string>;
    
    // 探索開始（非同期）
    startSearch(options: SearchOptions): Promise<void>;
    
    // 探索結果取得（ポーリング）
    getSearchResult(): Promise<SearchResult | null>;
    
    // 探索停止
    stopSearch(): void;
    
    // 局面設定
    setPosition(sfen: string, moves: string[]): void;
    
    // 評価値取得
    evaluatePosition(): number;
}

// Worker内での実装
class WasmEngineWorker implements WasmEngineService {
    private engine: WasmEngine | null = null;
    private searchInterval: number | null = null;

    async initialize(): Promise<void> {
        const wasmModule = await import('shogi-engine-wasm');
        this.engine = new wasmModule.WasmEngine();
        
        // エンジン初期化
        const response = await this.sendCommand('usi');
        await this.sendCommand('isready');
    }

    async sendCommand(command: string): Promise<string> {
        if (!this.engine) throw new Error('Engine not initialized');
        return this.engine.process_command(command);
    }

    async startSearch(options: SearchOptions): Promise<void> {
        // go コマンドを構築
        let goCommand = 'go';
        if (options.depth) goCommand += ` depth ${options.depth}`;
        if (options.time) goCommand += ` movetime ${options.time}`;
        if (options.nodes) goCommand += ` nodes ${options.nodes}`;
        
        await this.sendCommand(goCommand);
        
        // 結果ポーリング開始
        this.startPolling();
    }

    private startPolling(): void {
        this.searchInterval = setInterval(() => {
            const result = this.engine?.get_search_result();
            if (result) {
                this.stopPolling();
                self.postMessage({
                    type: 'searchComplete',
                    result: this.parseSearchResult(result)
                });
            }
        }, 100); // 100ms間隔でポーリング
    }
}
```

## 実装計画

### Phase 1: 基本インフラ（1週間）
1. **engine-wasmクレート作成**
   - Cargo.toml設定
   - wasm-bindgen依存関係
   - 基本的なビルド設定

2. **最小限のUSI実装**
   - usi, isready コマンド
   - position コマンド（SFEN解析）
   - 簡単なテスト

3. **TypeScript型定義**
   - WASMモジュールの型定義生成
   - インターフェース定義

### Phase 2: 探索機能実装（2週間）
1. **goコマンド実装**
   - 探索パラメータ解析
   - 非同期探索の開始
   - 結果フォーマット

2. **探索結果管理**
   - SearchHandleのWASMラッパー
   - ポーリングメカニズム
   - info文字列生成

3. **Worker統合**
   - 既存AIServiceとの互換性維持
   - WasmEngineWorkerの実装
   - メッセージプロトコル定義

### Phase 3: 高度な機能（2週間）
1. **オプション設定**
   - setoption実装
   - ハッシュサイズ、スレッド数
   - 評価関数切り替え

2. **評価機能**
   - 静的評価値取得
   - 詳細な局面分析
   - PV（主要変化）情報

3. **パフォーマンス最適化**
   - WASM SIMD有効化
   - メモリ管理改善
   - SharedArrayBuffer検討

### Phase 4: 統合とテスト（1週間）
1. **既存UIとの統合**
   - AIGameSetupコンポーネント更新
   - 難易度設定の対応
   - エンジン切り替えUI

2. **テスト**
   - 単体テスト（Rust側）
   - 統合テスト（TypeScript側）
   - パフォーマンステスト

3. **ドキュメント**
   - API仕様書
   - 使用例
   - トラブルシューティング

## 技術的課題と対策

### 1. 非同期処理
- **課題**: RustのSearcherは同期的、WebWorkerは非同期が必要
- **対策**: ポーリングメカニズムで擬似的な非同期を実現

### 2. メモリ管理
- **課題**: WASMのメモリ制限（4GB）
- **対策**: 
  - ハッシュテーブルサイズの適切な設定
  - 不要なデータの早期解放

### 3. パフォーマンス
- **課題**: JavaScript-WASM間の通信オーバーヘッド
- **対策**: 
  - バッチ処理の活用
  - 頻繁な状態更新を避ける
  - Worker内で完結する処理設計

### 4. デバッグ
- **課題**: WASMのデバッグは困難
- **対策**: 
  - 詳細なログ出力
  - Rust側での十分なテスト
  - source mapの活用

## 将来の拡張

1. **NNUE重みファイルの動的読み込み**
   - IndexedDBからの読み込み
   - 複数の評価関数の切り替え

2. **並列探索**
   - Web Workersでの並列化
   - SharedArrayBufferの活用

3. **学習機能**
   - 自己対戦データの収集
   - パラメータチューニング

4. **クラウド連携**
   - サーバーサイドエンジンとの協調
   - 分散探索

## まとめ

このWASM統合により、高性能なRust将棋エンジンをWebブラウザで直接実行可能になる。USIプロトコルのサブセットを採用することで、将来的な拡張性も確保しつつ、既存のTypeScriptコードとの互換性も維持できる。