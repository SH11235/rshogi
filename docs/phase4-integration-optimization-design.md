# Phase 4: 統合と最適化 - 詳細設計書

> **親ドキュメント**: [Rust将棋AI実装要件書](./rust-shogi-ai-requirements.md)  
> **該当セクション**: 9. 開発マイルストーン - Phase 4: 統合と最適化（2週間）  
> **前提条件**: [Phase 1](./phase1-foundation-design.md)、[Phase 2](./phase2-nnue-design.md)、[Phase 3](./phase3-search-enhancement-design.md) の完了

## 1. 概要

Phase 4では、これまでに開発したRust将棋AIエンジンをWebAssembly（WASM）として統合し、既存のTypeScriptアプリケーションから利用可能にします。また、ブラウザ環境での動作に向けた最適化と、教師データ生成機能の実装を行います。

### 1.1 目標
- WASM APIの実装と既存TypeScriptインターフェースとの統合
- ブラウザ環境向けの最適化（メモリ管理、SIMD対応）
- 教師データ生成機能の実装
- パフォーマンスチューニングと最終テスト

### 1.2 成果物
- `wasm_api.rs`: WASM公開API
- `js_interface.rs`: JavaScript型変換
- `self_play.rs`: 自己対戦・教師データ生成
- `wasm_optimization.rs`: WASM固有の最適化
- TypeScript型定義ファイル
- 統合テストスイート
- パフォーマンスベンチマーク

## 2. WASM API設計

### 2.1 公開インターフェース

```rust
use wasm_bindgen::prelude::*;
use serde::{Serialize, Deserialize};

/// WASM公開用のAIエンジン
#[wasm_bindgen]
pub struct WasmAIEngine {
    /// 内部エンジン
    engine: InternalEngine,
    /// 現在の局面
    position: Position,
    /// 探索スレッド（Web Worker）
    search_thread: Option<SearchHandle>,
}

/// 内部エンジン構造
struct InternalEngine {
    evaluator: NNUEEvaluatorWrapper,
    searcher: EnhancedSearcher,
    tt: Arc<TranspositionTable>,
    time_mgr: TimeManager,
}

/// JavaScript側の設定
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    /// 置換表サイズ（MB）
    pub hash_size: Option<usize>,
    /// スレッド数（Web Workerの数）
    pub threads: Option<usize>,
    /// NNUEファイルのパス
    pub eval_file: Option<String>,
    /// 探索パラメータ
    pub search_params: Option<SearchConfig>,
}

/// 探索設定
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfig {
    pub multi_pv: Option<u32>,
    pub contempt: Option<i32>,
    pub skill_level: Option<u32>,
}

/// 局面の表現（JSON互換）
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPosition {
    /// 盤面（9x9）
    pub board: Vec<Vec<Option<JsPiece>>>,
    /// 持ち駒
    pub hands: JsHands,
    /// 手番
    pub side_to_move: String, // "black" or "white"
    /// 手数
    pub ply: u32,
}

/// 駒の表現
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPiece {
    pub piece_type: String,
    pub owner: String,
    pub promoted: bool,
}

/// 持ち駒
#[derive(Serialize, Deserialize)]
pub struct JsHands {
    pub black: HashMap<String, u32>,
    pub white: HashMap<String, u32>,
}

/// 手の表現
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsMove {
    pub from: Option<JsSquare>,
    pub to: JsSquare,
    pub piece_type: String,
    pub promote: Option<bool>,
    pub drop_piece_type: Option<String>,  // 駒打ち用
}

/// マスの表現
#[derive(Serialize, Deserialize)]
pub struct JsSquare {
    pub row: u32,
    pub column: u32,
}

/// 探索結果
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub best_move: JsMove,
    pub score: i32,
    pub depth: u32,
    pub nodes: String, // 大きい数値のため文字列
    pub nps: u32,
    pub time: u32,
    pub pv: Vec<JsMove>,
    pub multi_pv: Option<Vec<PvInfo>>,
}

/// PV情報
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PvInfo {
    pub score: i32,
    pub depth: u32,
    pub pv: Vec<JsMove>,
}
```

### 2.2 WASM APIメソッド

```rust
#[wasm_bindgen]
impl WasmAIEngine {
    /// エンジンを作成
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<WasmAIEngine, JsValue> {
        // panicをJavaScriptエラーに変換
        console_error_panic_hook::set_once();
        
        let config: EngineConfig = config.into_serde()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        // 置換表作成
        let hash_size = config.hash_size.unwrap_or(16);
        let tt = Arc::new(TranspositionTable::new(hash_size));
        
        // 評価関数読み込み
        let evaluator = if let Some(path) = config.eval_file {
            load_nnue_from_bytes(&fetch_eval_file(&path)?)
                .map_err(|e| JsValue::from_str(&e.to_string()))?
        } else {
            // デフォルト評価関数（埋め込み）
            load_default_nnue()
                .map_err(|e| JsValue::from_str(&e.to_string()))?
        };
        
        let engine = InternalEngine {
            evaluator: NNUEEvaluatorWrapper::new(evaluator),
            searcher: EnhancedSearcher::new(Arc::clone(&tt)),
            tt,
            time_mgr: TimeManager::new(),
        };
        
        Ok(WasmAIEngine {
            engine,
            position: Position::startpos(),
            search_thread: None,
        })
    }
    
    /// 局面を設定
    #[wasm_bindgen(js_name = setPosition)]
    pub fn set_position(&mut self, position: JsValue) -> Result<(), JsValue> {
        let js_pos: JsPosition = position.into_serde()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        self.position = Self::js_to_internal_position(&js_pos)?;
        self.engine.evaluator.refresh_accumulator(&self.position);
        
        Ok(())
    }
    
    /// 手を実行
    #[wasm_bindgen(js_name = makeMove)]
    pub fn make_move(&mut self, js_move: JsValue) -> Result<(), JsValue> {
        let js_move: JsMove = js_move.into_serde()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let internal_move = Self::js_to_internal_move(&js_move)?;
        
        // 合法性チェック
        if !self.position.is_legal(internal_move) {
            return Err(JsValue::from_str("Illegal move"));
        }
        
        self.position.do_move(internal_move);
        self.engine.evaluator.do_move(&self.position, internal_move);
        
        Ok(())
    }
    
    /// 手を戻す
    #[wasm_bindgen(js_name = undoMove)]
    pub fn undo_move(&mut self) -> Result<(), JsValue> {
        if let Some(last_move) = self.position.last_move() {
            self.position.undo_move();
            self.engine.evaluator.undo_move();
            Ok(())
        } else {
            Err(JsValue::from_str("No move to undo"))
        }
    }
    
    /// 探索を開始（非同期）
    #[wasm_bindgen(js_name = search)]
    pub async fn search(
        &mut self,
        time_limit: u32,
        options: JsValue,
    ) -> Result<JsValue, JsValue> {
        let search_options: SearchOptions = options.into_serde()
            .unwrap_or_default();
        
        // 探索制限を設定
        let limits = SearchLimits {
            movetime: Some(Duration::from_millis(time_limit as u64)),
            depth: search_options.depth,
            nodes: search_options.nodes,
            ..Default::default()
        };
        
        // 非同期探索を実行
        let result = self.search_async(limits).await?;
        
        // JavaScript形式に変換
        let js_result = Self::internal_to_js_result(&result);
        
        Ok(JsValue::from_serde(&js_result)
            .map_err(|e| JsValue::from_str(&e.to_string()))?)
    }
    
    /// 探索を停止
    #[wasm_bindgen(js_name = stop)]
    pub fn stop(&mut self) {
        if let Some(handle) = &self.search_thread {
            handle.stop();
        }
    }
    
    /// 先読みが的中した場合
    #[wasm_bindgen(js_name = ponderHit)]
    pub fn ponder_hit(&mut self) {
        // 先読みから通常探索に切り替え
        if let Some(handle) = &self.search_thread {
            handle.ponder_hit();
        }
        self.engine.time_mgr.ponder_hit();
    }
    
    /// 現在の評価値を取得
    #[wasm_bindgen(js_name = evaluate)]
    pub fn evaluate(&self) -> i32 {
        self.engine.evaluator.evaluate(&self.position)
    }
    
    /// 合法手を生成
    #[wasm_bindgen(js_name = generateMoves)]
    pub fn generate_moves(&self) -> Result<JsValue, JsValue> {
        let mut movegen = MoveGen::new(&self.position);
        let moves = movegen.generate_all();
        
        let js_moves: Vec<JsMove> = moves.iter()
            .map(|&m| Self::internal_to_js_move(m))
            .collect();
        
        Ok(JsValue::from_serde(&js_moves)
            .map_err(|e| JsValue::from_str(&e.to_string()))?)
    }
    
    /// デバッグ情報を取得
    #[wasm_bindgen(js_name = getDebugInfo)]
    pub fn get_debug_info(&self) -> Result<JsValue, JsValue> {
        let info = DebugInfo {
            hash: format!("{:016x}", self.position.hash()),
            fen: self.position.to_sfen(),
            evaluation: self.evaluate(),
            features: self.get_active_features(),
            tt_usage: self.engine.tt.hashfull(),
        };
        
        Ok(JsValue::from_serde(&info)
            .map_err(|e| JsValue::from_str(&e.to_string()))?)
    }
    
    /// 探索を開始（同期版、デバッグ用）
    #[wasm_bindgen(js_name = searchSync)]
    pub fn search_sync(
        &mut self,
        time_limit: u32,
        options: JsValue,
    ) -> Result<JsValue, JsValue> {
        let search_options: SearchOptions = options.into_serde()
            .unwrap_or_default();
        
        // 探索制限を設定
        let limits = SearchLimits {
            movetime: Some(Duration::from_millis(time_limit as u64)),
            depth: search_options.depth,
            nodes: search_options.nodes,
            ..Default::default()
        };
        
        // 同期探索を実行
        let result = self.engine.searcher.iterative_deepening(&self.position, limits, 0);
        
        // JavaScript形式に変換
        let js_result = Self::internal_to_js_result(&result);
        
        Ok(JsValue::from_serde(&js_result)
            .map_err(|e| JsValue::from_str(&e.to_string()))?)
    }
}
```

### 2.3 型変換実装

```rust
impl WasmAIEngine {
    /// JavaScript形式から内部形式への変換
    fn js_to_internal_position(js_pos: &JsPosition) -> Result<Position, JsValue> {
        let mut pos = Position::empty();
        
        // 盤面を設定
        for (row, rank) in js_pos.board.iter().enumerate() {
            for (col, piece) in rank.iter().enumerate() {
                if let Some(js_piece) = piece {
                    let square = Square::new(col as u8, row as u8);
                    let piece = Self::js_to_internal_piece(js_piece)?;
                    pos.put_piece(square, piece);
                }
            }
        }
        
        // 持ち駒を設定
        for (piece_str, count) in &js_pos.hands.black {
            let piece_type = Self::parse_piece_type(piece_str)?;
            pos.add_hand(Color::Black, piece_type, *count as u8);
        }
        
        for (piece_str, count) in &js_pos.hands.white {
            let piece_type = Self::parse_piece_type(piece_str)?;
            pos.add_hand(Color::White, piece_type, *count as u8);
        }
        
        // 手番を設定
        pos.set_side_to_move(match js_pos.side_to_move.as_str() {
            "black" => Color::Black,
            "white" => Color::White,
            _ => return Err(JsValue::from_str("Invalid side to move")),
        });
        
        pos.set_ply(js_pos.ply as u16);
        
        Ok(pos)
    }
    
    /// 内部形式からJavaScript形式への変換
    fn internal_to_js_move(m: Move) -> JsMove {
        if m.is_drop() {
            JsMove {
                from: None,
                to: Self::square_to_js(m.to()),
                piece_type: Self::piece_type_to_string(m.drop_piece_type()),
                promote: None,
            }
        } else {
            JsMove {
                from: Some(Self::square_to_js(m.from())),
                to: Self::square_to_js(m.to()),
                piece_type: String::new(), // 移動の場合は不要
                promote: if m.is_promote() { Some(true) } else { None },
            }
        }
    }
    
    fn square_to_js(sq: Square) -> JsSquare {
        JsSquare {
            row: sq.rank() as u32,
            column: sq.file() as u32,
        }
    }
    
    fn piece_type_to_string(pt: PieceType) -> String {
        match pt {
            PieceType::King => "king".to_string(),
            PieceType::Rook => "rook".to_string(),
            PieceType::Bishop => "bishop".to_string(),
            PieceType::Gold => "gold".to_string(),
            PieceType::Silver => "silver".to_string(),
            PieceType::Knight => "knight".to_string(),
            PieceType::Lance => "lance".to_string(),
            PieceType::Pawn => "pawn".to_string(),
        }
    }
}
```

## 3. Web Worker統合

### 3.1 非同期探索実装

```rust
use wasm_bindgen_futures::spawn_local;
use web_sys::{Worker, MessageEvent};

/// 探索ハンドル
struct SearchHandle {
    worker: Worker,
    result_receiver: Receiver<InternalSearchResult>,
}

impl WasmAIEngine {
    /// 非同期探索
    async fn search_async(&mut self, limits: SearchLimits) -> Result<InternalSearchResult, JsValue> {
        // 既存の探索を停止
        if let Some(handle) = self.search_thread.take() {
            handle.stop();
        }
        
        // 新しいWorkerを作成
        let worker = Worker::new("./shogi-worker.js")?;
        let (tx, rx) = oneshot::channel();
        
        // メッセージハンドラを設定
        let tx = Rc::new(RefCell::new(Some(tx)));
        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Some(tx) = tx.borrow_mut().take() {
                let result: InternalSearchResult = e.data().into_serde().unwrap();
                tx.send(result).ok();
            }
        }) as Box<dyn FnMut(_)>);
        
        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
        
        // 探索コマンドを送信
        let command = SearchCommand {
            position: self.position.clone(),
            limits,
            tt_data: self.engine.tt.export_for_worker(),
            evaluator_data: self.engine.evaluator.export_weights(),
        };
        
        let msg = JsValue::from_serde(&command)?;
        worker.post_message(&msg)?;
        
        // ハンドルを保存
        self.search_thread = Some(SearchHandle { worker, result_receiver: rx });
        
        // 結果を待機
        match rx.await {
            Ok(result) => Ok(result),
            Err(_) => Err(JsValue::from_str("Search cancelled")),
        }
    }
}

/// Worker側の実装
#[wasm_bindgen]
pub struct SearchWorker {
    engine: Option<InternalEngine>,
}

#[wasm_bindgen]
impl SearchWorker {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        SearchWorker { engine: None }
    }
    
    /// メッセージ処理
    #[wasm_bindgen(js_name = handleMessage)]
    pub fn handle_message(&mut self, msg: JsValue) -> Result<JsValue, JsValue> {
        let command: SearchCommand = msg.into_serde()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        // エンジン初期化（初回のみ）
        if self.engine.is_none() {
            self.engine = Some(Self::create_engine(&command)?);
        }
        
        let engine = self.engine.as_mut().unwrap();
        
        // 探索実行
        let result = engine.searcher.iterative_deepening(
            &command.position,
            command.limits,
            0
        );
        
        Ok(JsValue::from_serde(&result)?)
    }
}
```

### 3.2 SharedArrayBuffer対応

#### CORSポリシー設定

```javascript
// サーバー側でのヘッダー設定
// SharedArrayBufferを有効にするには以下のヘッダーが必要
response.headers.set('Cross-Origin-Opener-Policy', 'same-origin');
response.headers.set('Cross-Origin-Embedder-Policy', 'require-corp');

// Service Workerでの対応例
self.addEventListener('fetch', (event) => {
  if (event.request.destination === 'document') {
    event.respondWith(
      fetch(event.request)
        .then((response) => {
          const newHeaders = new Headers(response.headers);
          newHeaders.set('Cross-Origin-Opener-Policy', 'same-origin');
          newHeaders.set('Cross-Origin-Embedder-Policy', 'require-corp');
          
          return new Response(response.body, {
            status: response.status,
            statusText: response.statusText,
            headers: newHeaders
          });
        })
    );
  }
});
```

```rust
/// 共有メモリを使用した高速化
#[cfg(feature = "shared-memory")]
mod shared_memory {
    use wasm_bindgen::prelude::*;
    use js_sys::SharedArrayBuffer;
    
    /// 共有置換表
    pub struct SharedTT {
        buffer: SharedArrayBuffer,
        view: js_sys::Uint8Array,
        size_mask: usize,
    }
    
    impl SharedTT {
        pub fn new(size_mb: usize) -> Result<Self, JsValue> {
            let size = (size_mb * 1024 * 1024).next_power_of_two();
            let buffer = SharedArrayBuffer::new(size as u32);
            let view = js_sys::Uint8Array::new(&buffer);
            
            Ok(SharedTT {
                buffer,
                view,
                size_mask: (size / std::mem::size_of::<TTEntry>()) - 1,
            })
        }
        
        pub fn probe(&self, hash: u64) -> Option<TTData> {
            let index = (hash as usize) & self.size_mask;
            let offset = index * std::mem::size_of::<TTEntry>();
            
            // Atomics APIを使用して読み込み
            let mut data = [0u8; std::mem::size_of::<TTEntry>()];
            for i in 0..data.len() {
                data[i] = Atomics::load(&self.view, (offset + i) as i32) as u8;
            }
            
            // TTEntryとして解釈
            let entry: &TTEntry = unsafe {
                std::mem::transmute(&data[0])
            };
            
            entry.load()
        }
        
        pub fn store(&self, hash: u64, data: TTData) {
            let index = (hash as usize) & self.size_mask;
            let offset = index * std::mem::size_of::<TTEntry>();
            
            // TTEntryを作成
            let entry = TTEntry::new(data);
            let bytes: &[u8; std::mem::size_of::<TTEntry>()] = unsafe {
                std::mem::transmute(&entry)
            };
            
            // Atomics APIを使用して書き込み
            for i in 0..bytes.len() {
                Atomics::store(&self.view, (offset + i) as i32, bytes[i] as i32);
            }
        }
    }
}
```

## 4. メモリ管理とサイズ最適化

### 4.1 動的メモリ管理

#### メモリ使用状況ログ

```rust
/// WASMメモリ統計情報
#[derive(Serialize)]
pub struct MemoryStats {
    pub linear_memory_used: usize,
    pub linear_memory_limit: usize,
    pub stack_size_estimate: usize,
    pub tt_size: usize,
    pub nnue_size: usize,
    pub misc_allocations: usize,
    pub timestamp: u64,
}

impl MemoryManager {
    /// メモリ使用状況を取得
    pub fn get_stats(&self) -> MemoryStats {
        MemoryStats {
            linear_memory_used: self.allocated.load(Ordering::Relaxed),
            linear_memory_limit: self.available_memory,
            stack_size_estimate: self.estimate_stack_usage(),
            tt_size: self.tt_allocation_size,
            nnue_size: self.nnue_allocation_size,
            misc_allocations: self.misc_allocations.load(Ordering::Relaxed),
            timestamp: js_sys::Date::now() as u64,
        }
    }
    
    /// CLIからのメモリ統計出力
    #[wasm_bindgen(js_name = logMemoryStats)]
    pub fn log_memory_stats(&self) {
        let stats = self.get_stats();
        web_sys::console::log_1(&JsValue::from_str(&serde_json::to_string(&stats).unwrap()));
    }
}
```

```rust
/// メモリマネージャー
pub struct MemoryManager {
    /// 利用可能メモリ（バイト）
    available_memory: usize,
    /// 割り当て済みメモリ
    allocated: AtomicUsize,
}

impl MemoryManager {
    pub fn new() -> Self {
        // ブラウザのメモリ制限を考慮
        let available = Self::detect_available_memory();
        
        MemoryManager {
            available_memory: available,
            allocated: AtomicUsize::new(0),
        }
    }
    
    fn detect_available_memory() -> usize {
        // WebAssembly.Memory の最大サイズを確認
        #[cfg(target_arch = "wasm32")]
        {
            // デフォルト: 256MB
            256 * 1024 * 1024
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            // ネイティブ: 1GB
            1024 * 1024 * 1024
        }
    }
    
    /// 置換表サイズを動的に決定
    pub fn calculate_tt_size(&self, requested_mb: usize) -> usize {
        let requested = requested_mb * 1024 * 1024;
        let available = self.available_memory - self.allocated.load(Ordering::Relaxed);
        
        // 利用可能メモリの50%まで
        let max_tt = available / 2;
        
        requested.min(max_tt)
    }
}

/// メモリ効率的な構造体
#[repr(C, packed)]
pub struct CompactMove {
    data: u16, // from(7) + to(7) + promote(1) + drop(1)
}

impl CompactMove {
    pub fn pack(m: Move) -> Self {
        let mut data = 0u16;
        
        if m.is_drop() {
            data |= 1 << 15;
            data |= (m.drop_piece_type() as u16) << 12;
            data |= m.to().0 as u16;
        } else {
            data |= (m.from().0 as u16) << 8;
            data |= m.to().0 as u16;
            if m.is_promote() {
                data |= 1 << 14;
            }
        }
        
        CompactMove { data }
    }
    
    pub fn unpack(self) -> Move {
        if self.data & (1 << 15) != 0 {
            // Drop
            let piece_type = ((self.data >> 12) & 0x7) as u8;
            let to = Square((self.data & 0x7F) as u8);
            Move::drop(unsafe { std::mem::transmute(piece_type) }, to)
        } else {
            // Normal
            let from = Square(((self.data >> 8) & 0x7F) as u8);
            let to = Square((self.data & 0x7F) as u8);
            let promote = self.data & (1 << 14) != 0;
            Move::normal(from, to, promote)
        }
    }
}
```

### 4.2 バイナリサイズ削減

```rust
// Cargo.toml の最適化設定
[profile.release]
opt-level = "z"     # サイズ最適化
lto = true          # Link Time Optimization
codegen-units = 1   # 単一コード生成ユニット
strip = true        # シンボル削除

[dependencies]
wasm-bindgen = { version = "0.2", features = ["serde-serialize"] }
serde = { version = "1.0", default-features = false, features = ["derive"] }
serde_json = { version = "1.0", default-features = false, features = ["alloc"] }

// wasm-opt による追加最適化
// wasm-opt -Oz -o optimized.wasm original.wasm

/// 条件付きコンパイル
#[cfg(feature = "minimal")]
mod minimal {
    // 最小構成（評価関数のみ）
    pub fn evaluate_position(pos: &Position) -> i32 {
        // NNUEのみ、探索なし
    }
}

#[cfg(not(feature = "minimal"))]
mod full {
    // フル機能
}
```

## 5. 教師データ生成

### 5.1 自己対戦システム

```rust
/// 自己対戦管理
pub struct SelfPlayManager {
    engine: Arc<InternalEngine>,
    config: SelfPlayConfig,
    output: Box<dyn TeacherWriter>,
}

/// 自己対戦設定
#[derive(Deserialize)]
pub struct SelfPlayConfig {
    /// 生成する局面数
    pub num_positions: u64,
    /// 探索深度
    pub search_depth: Depth,
    /// 探索ノード数
    pub search_nodes: Option<u64>,
    /// 開始局面のランダム手数
    pub random_moves: u32,
    /// 評価値フィルタ（デフォルト: ±800）
    pub eval_limit: Value,
    /// 出力形式
    pub output_format: TeacherFormat,
}

/// 教師データ形式
#[derive(Deserialize)]
pub enum TeacherFormat {
    /// やねうら王形式
    YaneuraOu,
    /// 独自バイナリ形式
    Binary,
    /// テキスト形式（デバッグ用）
    Text,
}

/// 教師データ
#[derive(Serialize)]
pub struct TeacherEntry {
    /// 局面（SFEN）
    pub sfen: String,
    /// 手番
    pub side_to_move: Color,
    /// 評価値
    pub eval: Value,
    /// 最善手
    pub best_move: Move,
    /// ゲームの結果
    pub game_result: GameResult,
    /// 手数
    pub ply: u16,
}

impl SelfPlayManager {
    pub fn new(
        engine: Arc<InternalEngine>,
        config: SelfPlayConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let output: Box<dyn TeacherWriter> = match config.output_format {
            TeacherFormat::YaneuraOu => Box::new(YaneuraOuWriter::new("teacher.bin")?),
            TeacherFormat::Binary => Box::new(BinaryWriter::new("teacher.dat")?),
            TeacherFormat::Text => Box::new(TextWriter::new("teacher.txt")?),
        };
        
        Ok(SelfPlayManager {
            engine,
            config,
            output,
        })
    }
    
    /// 自己対戦を実行
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut generated = 0u64;
        let mut rng = rand::thread_rng();
        
        while generated < self.config.num_positions {
            // 1ゲームを実行
            let entries = self.play_one_game(&mut rng).await?;
            
            // フィルタリング
            let mut filtered: Vec<_> = entries.into_iter()
                .filter(|e| e.eval.abs() <= self.config.eval_limit) // デフォルト: ±800
                .filter(|e| e.ply >= 20 && e.ply <= 200)
                .collect();
            
            // ゲーム結果を正確に設定
            self.update_game_results(&mut filtered);
            
            // 書き込み
            for entry in filtered {
                self.output.write(&entry)?;
                generated += 1;
                
                if generated % 10000 == 0 {
                    web_sys::console::log_1(
                        &format!("Generated {} positions", generated).into()
                    );
                }
            }
        }
        
        self.output.flush()?;
        Ok(())
    }
    
    /// ゲーム結果の更新
    fn update_game_results(&self, entries: &mut Vec<TeacherEntry>) {
        // ゲームの最終結果を取得
        let final_result = if let Some(last) = entries.last() {
            // 詰み判定
            if last.eval.abs() > VALUE_KNOWN_WIN {
                if last.eval > 0 {
                    GameResult::Win(last.side_to_move)
                } else {
                    GameResult::Win(last.side_to_move.opposite())
                }
            } else if last.ply >= 320 {
                // 千日手判定（320手以上）
                GameResult::Draw
            } else {
                // 通常の終了
                GameResult::Unknown
            }
        } else {
            GameResult::Unknown
        };
        
        // 全てのエントリにゲーム結果を設定
        for entry in entries.iter_mut() {
            entry.game_result = final_result;
        }
    }
    
    /// 1ゲームをプレイ
    async fn play_one_game(
        &self,
        rng: &mut impl Rng,
    ) -> Result<Vec<TeacherEntry>, Box<dyn std::error::Error>> {
        let mut pos = Position::startpos();
        let mut entries = Vec::new();
        let mut move_history = Vec::new();
        
        // 開始局面のランダム化
        for _ in 0..self.config.random_moves {
            let moves = self.get_legal_moves(&pos);
            if moves.is_empty() {
                break;
            }
            
            let random_move = moves[rng.random_range(0..moves.len())];
            pos.do_move(random_move);
            move_history.push(random_move);
        }
        
        // ゲーム本体
        let max_moves = 512;
        for i in 0..max_moves {
            // 千日手チェック
            if pos.is_repetition() {
                break;
            }
            
            // 探索
            let limits = SearchLimits {
                depth: Some(self.config.search_depth),
                nodes: self.config.search_nodes,
                ..Default::default()
            };
            
            let result = self.search(&pos, limits).await?;
            
            // 教師データ作成
            let entry = TeacherEntry {
                sfen: pos.to_sfen(),
                side_to_move: pos.side_to_move,
                eval: result.score,
                best_move: result.best_move,
                game_result: GameResult::Unknown, // 後で更新
                ply: pos.ply,
            };
            entries.push(entry);
            
            // 手を実行
            pos.do_move(result.best_move);
            move_history.push(result.best_move);
            
            // 詰みチェック
            if result.score.abs() > VALUE_KNOWN_WIN {
                break;
            }
        }
        
        // ゲーム結果を遡って更新
        let game_result = self.determine_game_result(&pos);
        for entry in &mut entries {
            entry.game_result = game_result;
        }
        
        Ok(entries)
    }
}
```

### 5.2 教師データ形式

```rust
/// やねうら王形式ライター
struct YaneuraOuWriter {
    file: std::fs::File,
    buffer: Vec<u8>,
}

impl YaneuraOuWriter {
    fn new(path: &str) -> std::io::Result<Self> {
        Ok(YaneuraOuWriter {
            file: std::fs::File::create(path)?,
            buffer: Vec::with_capacity(1024 * 1024),
        })
    }
}

impl TeacherWriter for YaneuraOuWriter {
    fn write(&mut self, entry: &TeacherEntry) -> std::io::Result<()> {
        // packed sfen (256bit)
        let packed_sfen = pack_sfen(&entry.sfen);
        self.buffer.extend_from_slice(&packed_sfen);
        
        // move (16bit)
        let move_data = entry.best_move.to_u16();
        self.buffer.extend_from_slice(&move_data.to_le_bytes());
        
        // eval (16bit)
        let eval = entry.eval as i16;
        self.buffer.extend_from_slice(&eval.to_le_bytes());
        
        // game result (8bit)
        let result = match entry.game_result {
            GameResult::BlackWin => 1u8,
            GameResult::WhiteWin => -1i8 as u8,
            GameResult::Draw => 0u8,
            GameResult::Unknown => 2u8,
        };
        self.buffer.push(result);
        
        // padding (8bit)
        self.buffer.push(0);
        
        // バッファがいっぱいになったら書き込み
        if self.buffer.len() >= 1024 * 1024 {
            self.flush()?;
        }
        
        Ok(())
    }
    
    fn flush(&mut self) -> std::io::Result<()> {
        use std::io::Write;
        self.file.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }
}
```

## 6. パフォーマンス最適化

### 6.1 WASM固有の最適化

```rust
/// WASM SIMD対応 (simd128)
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod wasm_simd {
    use std::arch::wasm32::*;
    
    /// SIMD対応のドット積
    pub unsafe fn dot_product_simd(a: &[i8], b: &[i8], len: usize) -> i32 {
        let mut sum = i32x4_splat(0);
        
        for i in (0..len).step_by(16) {
            // 16要素を一度に処理
            let a_vec = v128_load(a.as_ptr().add(i) as *const v128);
            let b_vec = v128_load(b.as_ptr().add(i) as *const v128);
            
            // 8ビット整数の積和演算
            let prod = i16x8_extmul_low_i8x16(a_vec, b_vec);
            sum = i32x4_add(sum, i32x4_extadd_pairwise_i16x8(prod));
            
            let prod_high = i16x8_extmul_high_i8x16(a_vec, b_vec);
            sum = i32x4_add(sum, i32x4_extadd_pairwise_i16x8(prod_high));
        }
        
        // 水平加算
        i32x4_extract_lane::<0>(sum) + 
        i32x4_extract_lane::<1>(sum) +
        i32x4_extract_lane::<2>(sum) +
        i32x4_extract_lane::<3>(sum)
    }
    
    /// SIMD機能検出
    pub fn is_simd_available() -> bool {
        // WebAssembly.validate() で確認
        true // 実際にはJavaScript側で確認
    }
}

/// パフォーマンスカウンター
#[wasm_bindgen]
pub struct PerformanceStats {
    search_time: f64,
    eval_time: f64,
    move_gen_time: f64,
    total_nodes: u64,
}

#[wasm_bindgen]
impl PerformanceStats {
    #[wasm_bindgen(getter)]
    pub fn nps(&self) -> u32 {
        if self.search_time > 0.0 {
            (self.total_nodes as f64 / self.search_time) as u32
        } else {
            0
        }
    }
    
    #[wasm_bindgen(getter)]
    pub fn eval_per_second(&self) -> u32 {
        if self.eval_time > 0.0 {
            (self.total_nodes as f64 / self.eval_time) as u32
        } else {
            0
        }
    }
}
```

### 6.2 プロファイリングとベンチマーク

```rust
/// ベンチマークスイート
#[wasm_bindgen]
pub struct Benchmark {
    positions: Vec<Position>,
    engine: WasmAIEngine,
}

#[wasm_bindgen]
impl Benchmark {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Benchmark, JsValue> {
        let positions = vec![
            Position::startpos(),
            Position::from_sfen("4k4/9/4P4/9/9/9/9/9/4K4 b - 1").unwrap(),
            Position::from_sfen("ln4knl/4g2r1/2sppps1p/2p3p2/PP2P4/2P3P1P/2NPPSP2/2S1GGK2/L1+R3BNL w Bb 1").unwrap(),
        ];
        
        let engine = WasmAIEngine::new(JsValue::NULL)?;
        
        Ok(Benchmark { positions, engine })
    }
    
    /// パフォーマンステストを実行
    #[wasm_bindgen(js_name = runPerformanceTest)]
    pub async fn run_performance_test(&mut self) -> Result<JsValue, JsValue> {
        let mut results = BenchmarkResults::default();
        
        // 各種ベンチマーク
        results.move_generation = self.bench_move_generation()?;
        results.evaluation = self.bench_evaluation()?;
        results.search_depth_10 = self.bench_search(10).await?;
        results.search_time_1s = self.bench_search_time(1000).await?;
        
        Ok(JsValue::from_serde(&results)?)
    }
    
    fn bench_move_generation(&self) -> Result<MoveGenBenchResult, JsValue> {
        let start = performance::now();
        let mut total_moves = 0;
        let iterations = 10000;
        
        for _ in 0..iterations {
            for pos in &self.positions {
                let mut movegen = MoveGen::new(pos);
                let moves = movegen.generate_all();
                total_moves += moves.len();
            }
        }
        
        let elapsed = performance::now() - start;
        
        Ok(MoveGenBenchResult {
            time_ms: elapsed,
            total_moves,
            moves_per_second: (total_moves as f64 / elapsed * 1000.0) as u32,
        })
    }
    
    fn bench_evaluation(&mut self) -> Result<EvalBenchResult, JsValue> {
        let start = performance::now();
        let iterations = 100000;
        
        for _ in 0..iterations {
            for pos in &self.positions {
                self.engine.position = pos.clone();
                let _ = self.engine.evaluate();
            }
        }
        
        let elapsed = performance::now() - start;
        
        Ok(EvalBenchResult {
            time_ms: elapsed,
            positions: iterations * self.positions.len(),
            evals_per_second: (iterations * self.positions.len()) as f64 / elapsed * 1000.0,
        })
    }
}

/// パフォーマンス監視
#[wasm_bindgen]
pub fn enable_performance_monitoring() {
    web_sys::console::time_with_label("search");
}

#[wasm_bindgen]
pub fn get_performance_report() -> Result<JsValue, JsValue> {
    web_sys::console::time_end_with_label("search");
    
    // パフォーマンスエントリを取得
    let window = web_sys::window().unwrap();
    let performance = window.performance().unwrap();
    let entries = performance.get_entries_by_type("measure");
    
    Ok(entries)
}
```

## 7. 統合テスト

### 7.1 TypeScriptインターフェーステスト

```typescript
// tests/integration.test.ts
import { WasmAIEngine } from '../pkg/shogi_ai';
import { beforeAll, describe, test, expect } from '@jest/globals';

describe('WASM AI Engine Integration', () => {
    let engine: WasmAIEngine;
    
    beforeAll(async () => {
        // WASMモジュールを初期化
        await init();
        engine = new WasmAIEngine({
            hashSize: 16,
            threads: 1,
        });
    });
    
    test('初期局面の設定', () => {
        const position = {
            board: createInitialBoard(),
            hands: { black: {}, white: {} },
            sideToMove: 'black',
            ply: 0,
        };
        
        expect(() => engine.setPosition(position)).not.toThrow();
    });
    
    test('合法手生成', () => {
        const moves = engine.generateMoves();
        expect(moves).toHaveLength(30); // 初期局面
    });
    
    test('探索実行', async () => {
        const result = await engine.search(1000, {
            depth: 10,
        });
        
        expect(result.bestMove).toBeDefined();
        expect(result.depth).toBeGreaterThanOrEqual(10);
        expect(result.score).toBeGreaterThanOrEqual(-1000);
        expect(result.score).toBeLessThanOrEqual(1000);
    });
    
    test('手の実行と取り消し', () => {
        const initialEval = engine.evaluate();
        
        const move = {
            from: { row: 6, column: 7 },
            to: { row: 5, column: 7 },
        };
        
        engine.makeMove(move);
        const afterMoveEval = engine.evaluate();
        
        engine.undoMove();
        const afterUndoEval = engine.evaluate();
        
        expect(afterMoveEval).not.toBe(initialEval);
        expect(afterUndoEval).toBe(initialEval);
    });
});
```

### 7.2 互換性テスト

```rust
#[cfg(test)]
mod compatibility_tests {
    use super::*;
    
    #[test]
    fn test_typescript_interface_compatibility() {
        // 既存のTypeScript AIEngineInterfaceとの互換性
        let engine = WasmAIEngine::new(JsValue::NULL).unwrap();
        
        // 同じメソッドが存在することを確認
        assert!(engine.set_position(create_test_position()).is_ok());
        assert!(engine.make_move(create_test_move()).is_ok());
        assert!(engine.evaluate() != 0);
        assert!(engine.generate_moves().is_ok());
    }
    
    #[test]
    fn test_json_serialization() {
        // JavaScript形式との相互変換
        let internal_move = Move::normal(Square::new(7, 6), Square::new(7, 5), false);
        let js_move = WasmAIEngine::internal_to_js_move(internal_move);
        
        let json = serde_json::to_string(&js_move).unwrap();
        let parsed: JsMove = serde_json::from_str(&json).unwrap();
        
        assert_eq!(parsed.from.unwrap().row, 6);
        assert_eq!(parsed.from.unwrap().column, 7);
        assert_eq!(parsed.to.row, 5);
        assert_eq!(parsed.to.column, 7);
    }
}
```

## 8. 実装スケジュール

### Week 1: WASM統合
- Day 1-2: WASM API実装
- Day 3: 型変換とJavaScriptインターフェース
- Day 4: Web Worker統合
- Day 5: メモリ管理実装
- Day 6-7: 統合テスト

### Week 2: 最適化と機能追加
- Day 1-2: WASM最適化（SIMD、サイズ削減）
- Day 3-4: 教師データ生成機能
- Day 5: パフォーマンスチューニング
- Day 6: ベンチマークスイート
- Day 7: 最終テストとドキュメント

## 9. 成功基準

### 機能要件
- [ ] 既存TypeScriptインターフェースとの完全互換
- [ ] Web Workerでの非同期探索
- [ ] 教師データ生成機能
- [ ] メモリ効率的な動作

### 性能要件
- [ ] 探索速度: 30-50万NPS（ブラウザ環境、WASM simd128使用時）
- [ ] WASMサイズ: 1MB以下（gzip圧縮後、wee_alloc + wasm-opt -Oz使用前提）
- [ ] 起動時間: 100ms以下（NNUE重みの事前fetchとキャッシュからの読み込みを除く）
- [ ] メモリ使用: 128MB以下
- [ ] WASM性能: ネイティブの70-80%

### 品質要件
- [ ] TypeScript型定義の完備
- [ ] エラーハンドリング
- [ ] 包括的なテストスイート

## 10. リスクと対策

### 技術的リスク
1. **ブラウザ制限**
   - 対策: 段階的な機能削減
   - フォールバック実装

2. **WASM性能**
   - 対策: プロファイリングによる最適化
   - critical pathの特定

3. **メモリ制約**
   - 対策: 動的メモリ管理
   - 必要に応じた機能制限

## 11. 追加推奨事項

### 11.1 型定義のnpm配布

```json
// package.json
{
  "name": "@shogi-ai/types",
  "version": "1.0.0",
  "types": "index.d.ts",
  "files": ["*.d.ts"],
  "scripts": {
    "build": "wasm-pack build --target web --out-dir pkg",
    "prepublishOnly": "npm run build"
  }
}
```

### 11.2 WebWorker thread-affinity

```typescript
// スレッド数の適切な決定
function getOptimalThreadCount(): number {
  const cores = navigator.hardwareConcurrency || 4;
  // UIスレッド用に1つ残し、1-4にクリップ
  return Math.max(1, Math.min(cores - 1, 4));
}
```

### 11.3 セキュリティレビュー

```rust
// エラーレポートフック
#[wasm_bindgen]
pub fn set_error_reporter(callback: js_sys::Function) {
    panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(&msg));
    }));
}

// Sentry等へのレポート
// JavaScript側でフックを設定
engine.set_error_reporter((error) => {
  Sentry.captureException(new Error(error));
});
```

## 12. プロジェクト完了チェックリスト

### 実装完了
- [ ] 全フェーズの実装完了
- [ ] 単体テスト（カバレッジ90%以上）
- [ ] 統合テスト
- [ ] パフォーマンステスト

### ドキュメント
- [ ] APIドキュメント
- [ ] 使用ガイド
- [ ] パフォーマンスレポート
- [ ] 今後の改善案

### デプロイメント
- [ ] npmパッケージ準備
- [ ] CDN配信準備
- [ ] サンプルアプリケーション
- [ ] リリースノート

これで、Rust将棋AIエンジンの開発が完了します。
