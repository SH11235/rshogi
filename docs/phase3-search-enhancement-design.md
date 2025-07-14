# Phase 3: 探索強化 - 詳細設計書

> **親ドキュメント**: [Rust将棋AI実装要件書](./rust-shogi-ai-requirements.md)  
> **該当セクション**: 9. 開発マイルストーン - Phase 3: 探索強化（2週間）  
> **前提条件**: [Phase 1: 基盤実装](./phase1-foundation-design.md)、[Phase 2: NNUE実装](./phase2-nnue-design.md) の完了

## 1. 概要

Phase 3では、基本的なαβ探索に最新の探索技術を追加し、並列化により探索速度を大幅に向上させます。これにより、より深い読みと精度の高い手の選択が可能になります。

### 1.1 目標
- 高度な枝刈り技術の実装（Null Move、LMR、Singular Extension等）
- Lazy SMPによる並列探索（最大16スレッド）
- 置換表の最適化とロックフリー実装
- 動的時間管理システム

### 1.2 成果物
- `search_enhanced.rs`: 拡張探索エンジン
- `tt_concurrent.rs`: 並列対応置換表
- `thread_pool.rs`: 探索スレッド管理
- `time_management.rs`: 時間管理システム
- `history.rs`: 履歴ヒューリスティック
- 並列性能テストスイート

## 2. 高度な探索技術

### 2.1 探索フレームワークの拡張

```rust
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// 探索スタック（各探索ノードの情報）
pub struct SearchStack {
    /// 現在の手
    current_move: Option<Move>,
    /// 継続手（過去の手の履歴）
    continuation_history: [Option<Move>; 8],
    /// 静的評価値
    static_eval: Value,
    /// キラー手
    killers: [Option<Move>; 2],
    /// 探索中の手数
    move_count: u32,
    /// PVノードかどうか
    pv: bool,
    /// Null moveを試したか
    null_move_tried: bool,
    /// 除外する手（Singular Extension用）
    excluded_move: Option<Move>,
}

/// 拡張探索エンジン
pub struct EnhancedSearcher {
    /// 置換表
    tt: Arc<TranspositionTable>,
    /// 履歴テーブル
    history: History,
    /// 時間管理
    time_mgr: TimeManager,
    /// 探索情報
    info: SearchInfo,
    /// 探索スタック
    stack: Vec<SearchStack>,
    /// 選択的深さ（実際に到達した最大深さ）
    selective_depth: i32,
}

/// 探索パラメータ
pub struct SearchParams {
    /// Null Move Pruning
    pub null_move_enabled: bool,
    pub null_move_reduction: fn(Depth) -> Depth,
    
    /// Late Move Reductions
    pub lmr_enabled: bool,
    pub lmr_table: [[Depth; 64]; 64], // [depth][move_count]
    
    /// Futility Pruning
    pub futility_margin: fn(Depth) -> Value,
    
    /// Aspiration Window
    pub aspiration_window_delta: Value,
    
    /// その他のパラメータ
    pub iid_depth: Depth,           // Internal Iterative Deepening
    pub singular_extension_depth: Depth,
    pub prob_cut_depth: Depth,
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            null_move_enabled: true,
            null_move_reduction: |depth| 3 + depth / 6,
            lmr_enabled: true,
            lmr_table: Self::init_lmr_table(),
            futility_margin: |depth| 75 * depth as Value,
            aspiration_window_delta: 17,
            iid_depth: 4,
            singular_extension_depth: 7,
            prob_cut_depth: 5,
        }
    }
}
```

### 2.2 PVS探索（Principal Variation Search）

```rust
impl EnhancedSearcher {
    /// PVS探索のメインループ
    pub fn search(
        &mut self,
        pos: &Position,
        mut alpha: Value,
        mut beta: Value,
        mut depth: Depth,
        cut_node: bool,
    ) -> Value {
        let pv_node = beta - alpha > 1;
        let root_node = self.stack.len() == 1;
        let in_check = pos.in_check();
        
        // 探索情報の更新
        self.info.nodes.fetch_add(1, Ordering::Relaxed);
        self.selective_depth = self.selective_depth.max(self.stack.len() as i32);
        
        // 探索停止チェック
        if self.should_stop() {
            return VALUE_DRAW;
        }
        
        // 深さ0または静止探索へ
        if depth <= 0 {
            return self.quiescence_search(pos, alpha, beta);
        }
        
        // 千日手チェック
        if !root_node && pos.is_repetition() {
            return VALUE_DRAW;
        }
        
        // 置換表の参照
        let tt_hit = self.tt.probe(pos.hash());
        let tt_move = tt_hit.as_ref().map(|e| e.best_move);
        let tt_value = tt_hit.as_ref().map(|e| e.value);
        let tt_eval = tt_hit.as_ref().map(|e| e.eval);
        
        // 置換表による枝刈り
        if !pv_node && tt_hit.is_some() {
            let tte = tt_hit.as_ref().unwrap();
            if tte.depth >= depth {
                let value = tte.value;
                if (tte.bound == Bound::Exact) ||
                   (tte.bound == Bound::Lower && value >= beta) ||
                   (tte.bound == Bound::Upper && value <= alpha) {
                    return value;
                }
            }
        }
        
        // 静的評価
        let eval = if in_check {
            VALUE_NONE
        } else {
            tt_eval.unwrap_or_else(|| self.evaluate(pos))
        };
        
        let improving = !in_check && self.stack.len() >= 2 &&
                        eval > self.stack[self.stack.len() - 2].static_eval;
        
        self.stack.last_mut().unwrap().static_eval = eval;
        
        // 枝刈り技術の適用
        if !pv_node && !in_check {
            // Null Move Pruning
            if self.null_move_pruning(pos, beta, depth, eval, cut_node) {
                return beta;
            }
            
            // ProbCut
            if let Some(value) = self.prob_cut(pos, beta, depth) {
                return value;
            }
            
            // Futility Pruning
            if depth < 7 && eval - self.params.futility_margin(depth) >= beta {
                return eval;
            }
        }
        
        // Internal Iterative Deepening (IID)
        if tt_move.is_none() && depth >= self.params.iid_depth {
            depth -= 1;
        }
        
        // 合法手生成と順序付け
        let mut mp = MovePicker::new(pos, tt_move, &self.history, self.stack.last().unwrap());
        let mut best_value = -VALUE_INFINITE;
        let mut best_move = Move::null();
        let mut move_count = 0;
        
        // 各手を探索
        while let Some(move_) = mp.next_move() {
            if move_ == self.stack.last().unwrap().excluded_move {
                continue;
            }
            
            move_count += 1;
            
            // 枝刈りとリダクション
            let (new_depth, do_full_search) = self.calculate_new_depth(
                pos, move_, depth, move_count, pv_node, improving, in_check
            );
            
            // 手を実行
            let mut new_pos = pos.clone();
            new_pos.do_move(move_);
            
            self.stack.push(SearchStack::new(move_));
            
            // 探索
            let value = if move_count == 1 {
                // 最初の手は通常探索
                -self.search(&new_pos, -beta, -alpha, new_depth, false)
            } else {
                // Late Move Reduction
                let reduced_depth = if do_full_search { new_depth } else { new_depth - 1 };
                let value = -self.search(&new_pos, -alpha - 1, -alpha, reduced_depth, true);
                
                if value > alpha && reduced_depth < new_depth {
                    // 再探索
                    -self.search(&new_pos, -beta, -alpha, new_depth, false)
                } else {
                    value
                }
            };
            
            self.stack.pop();
            
            // 最善手の更新
            if value > best_value {
                best_value = value;
                best_move = move_;
                
                if value > alpha {
                    alpha = value;
                    
                    if alpha >= beta {
                        // ベータカット
                        if !move_.is_capture() {
                            self.update_quiet_stats(pos, move_, depth);
                        }
                        break;
                    }
                }
            }
        }
        
        // 合法手がない場合
        if move_count == 0 {
            if self.stack.last().unwrap().excluded_move.is_some() {
                // Singular Extension中
                return alpha;
            }
            return if in_check { mated_in(self.stack.len()) } else { VALUE_DRAW };
        }
        
        // 置換表に保存
        let bound = if best_value >= beta { 
            Bound::Lower 
        } else if best_value > alpha { 
            Bound::Exact 
        } else { 
            Bound::Upper 
        };
        
        self.tt.store(pos.hash(), best_move, best_value, eval, depth, bound);
        
        best_value
    }
}
```

### 2.3 Null Move Pruning

```rust
impl EnhancedSearcher {
    fn null_move_pruning(
        &mut self,
        pos: &Position,
        beta: Value,
        depth: Depth,
        eval: Value,
        cut_node: bool,
    ) -> bool {
        if !self.params.null_move_enabled || 
           self.stack.last().unwrap().null_move_tried ||
           pos.has_few_pieces() {
            return false;
        }
        
        // 評価値が十分高い場合のみ
        if eval < beta {
            return false;
        }
        
        // 削減量の計算
        let r = self.params.null_move_reduction(depth);
        let null_depth = (depth - r).max(1);
        
        // Null moveを実行
        let mut null_pos = pos.clone();
        null_pos.do_null_move();
        
        self.stack.last_mut().unwrap().null_move_tried = true;
        self.stack.push(SearchStack::new(Move::null()));
        
        let null_value = -self.search(&null_pos, -beta, -beta + 1, null_depth, !cut_node);
        
        self.stack.pop();
        self.stack.last_mut().unwrap().null_move_tried = false;
        
        if null_value >= beta {
            // 検証探索（zugzwang対策）
            if depth > 12 && null_value > VALUE_KNOWN_WIN {
                let verification_value = self.search(pos, beta - 1, beta, depth - r, false);
                if verification_value >= beta {
                    return true;
                }
            } else {
                return true;
            }
        }
        
        false
    }
}
```

### 2.4 Late Move Reductions (LMR)

```rust
impl EnhancedSearcher {
    fn calculate_new_depth(
        &self,
        pos: &Position,
        move_: Move,
        depth: Depth,
        move_count: u32,
        pv_node: bool,
        improving: bool,
        in_check: bool,
    ) -> (Depth, bool) {
        let mut new_depth = depth - 1;
        let mut do_full_search = true;
        
        // 延長
        let mut extension = 0;
        
        // 王手延長
        if in_check {
            extension = 1;
        } else if move_count == 1 {
            // Singular Extension
            if let Some(se) = self.singular_extension(pos, move_, depth) {
                extension = se;
            }
        }
        
        new_depth += extension;
        
        // Late Move Reductions
        if depth >= 3 && move_count > 1 && !move_.is_capture() {
            // 基本削減量
            let mut r = self.params.lmr_table[depth.min(63) as usize][move_count.min(63) as usize];
            
            // 調整
            if !pv_node {
                r += 1;
            }
            if !improving {
                r += 1;
            }
            if cut_node {
                r += 2;
            }
            
            // 履歴による調整
            let history_score = self.history.get(pos, move_);
            if history_score < -1000 {
                r += 1;
            } else if history_score > 1000 {
                r -= 1;
            }
            
            // 削減量を適用
            new_depth = (new_depth - r as i32).max(1);
            do_full_search = false;
        }
        
        (new_depth, do_full_search)
    }
}
```

### 2.5 Singular Extension

```rust
impl EnhancedSearcher {
    fn singular_extension(
        &mut self,
        pos: &Position,
        tt_move: Move,
        depth: Depth,
    ) -> Option<Depth> {
        if depth < self.params.singular_extension_depth {
            return None;
        }
        
        // TTから情報取得
        let tt_hit = self.tt.probe(pos.hash())?;
        if tt_hit.bound != Bound::Lower || tt_hit.depth < depth - 3 {
            return None;
        }
        
        let singular_beta = tt_hit.value - 2 * depth as Value;
        let singular_depth = (depth - 1) / 2;
        
        // 他の手を浅く探索
        self.stack.last_mut().unwrap().excluded_move = Some(tt_move);
        let value = self.search(pos, singular_beta - 1, singular_beta, singular_depth, true);
        self.stack.last_mut().unwrap().excluded_move = None;
        
        if value < singular_beta {
            // この手は特別に良い
            if value < singular_beta - 50 {
                Some(2) // 二重延長
            } else {
                Some(1) // 単一延長
            }
        } else {
            // Multi-cut
            if singular_beta >= beta {
                self.stack.last_mut().unwrap().multi_cut = true;
            }
            None
        }
    }
}
```

## 3. 履歴ヒューリスティック

### 3.1 履歴テーブル構造

```rust
/// 履歴情報を管理
pub struct History {
    /// Butterfly History [color][from][to]
    butterfly: [[[AtomicI16; 81]; 81]; 2],
    
    /// Capture History [color][piece][to][captured]
    capture: [[[[AtomicI16; 8]; 81]; 14]; 2],
    
    /// Continuation History（過去の手との関係）
    continuation: Vec<ContinuationHistory>,
}

/// 継続履歴
pub struct ContinuationHistory {
    table: Box<[[[[AtomicI16; 81]; 81]; 14]; 14]>, // [piece1][to1][piece2][to2]
}

impl History {
    pub fn new() -> Self {
        History {
            butterfly: unsafe { std::mem::zeroed() },
            capture: unsafe { std::mem::zeroed() },
            continuation: vec![ContinuationHistory::new(); 8],
        }
    }
    
    /// 履歴スコアを取得
    pub fn get(&self, pos: &Position, move_: Move) -> i32 {
        let us = pos.side_to_move as usize;
        let from = move_.from().0 as usize;
        let to = move_.to().0 as usize;
        
        let mut score = self.butterfly[us][from][to].load(Ordering::Relaxed) as i32;
        
        // Capture history
        if let Some(captured) = move_.captured_piece() {
            let piece = pos.piece_on(move_.from()).unwrap();
            score += self.capture[us][piece.to_index()][to][captured.to_index()]
                .load(Ordering::Relaxed) as i32;
        }
        
        score
    }
    
    /// 成功した手の履歴を更新
    pub fn update_quiet(&self, pos: &Position, move_: Move, bonus: i32) {
        let us = pos.side_to_move as usize;
        let from = move_.from().0 as usize;
        let to = move_.to().0 as usize;
        
        // Butterfly history更新
        self.update_entry(&self.butterfly[us][from][to], bonus);
    }
    
    /// 履歴エントリを更新（飽和カウンタ）
    fn update_entry(&self, entry: &AtomicI16, bonus: i32) {
        let old = entry.load(Ordering::Relaxed) as i32;
        let new = old + bonus - old * bonus.abs() / 16384;
        let new = new.clamp(-16384, 16384) as i16;
        entry.store(new, Ordering::Relaxed);
    }
}
```

### 3.2 手の順序付け

```rust
/// 手の順序付けを行うクラス
pub struct MovePicker {
    pos: Position,
    tt_move: Option<Move>,
    history: Arc<History>,
    stage: MovePickerStage,
    moves: Vec<ScoredMove>,
    current: usize,
}

#[derive(Debug, Clone, Copy)]
struct ScoredMove {
    move_: Move,
    score: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MovePickerStage {
    TTMove,
    GenerateCaptures,
    GoodCaptures,
    Killers,
    GenerateQuiets,
    AllMoves,
    BadCaptures,
}

impl MovePicker {
    pub fn new(
        pos: &Position,
        tt_move: Option<Move>,
        history: &Arc<History>,
        ss: &SearchStack,
    ) -> Self {
        MovePicker {
            pos: pos.clone(),
            tt_move,
            history: Arc::clone(history),
            stage: MovePickerStage::TTMove,
            moves: Vec::new(),
            current: 0,
        }
    }
    
    pub fn next_move(&mut self) -> Option<Move> {
        loop {
            match self.stage {
                MovePickerStage::TTMove => {
                    self.stage = MovePickerStage::GenerateCaptures;
                    if let Some(tt_move) = self.tt_move {
                        if self.pos.is_legal(tt_move) {
                            return Some(tt_move);
                        }
                    }
                }
                
                MovePickerStage::GenerateCaptures => {
                    self.generate_captures();
                    self.score_captures();
                    self.stage = MovePickerStage::GoodCaptures;
                }
                
                MovePickerStage::GoodCaptures => {
                    if let Some(move_) = self.pick_best() {
                        if move_ != self.tt_move {
                            return Some(move_);
                        }
                    } else {
                        self.stage = MovePickerStage::Killers;
                    }
                }
                
                // ... 他のステージ
                
                MovePickerStage::AllMoves => {
                    if let Some(move_) = self.pick_best() {
                        if move_ != self.tt_move {
                            return Some(move_);
                        }
                    } else {
                        return None;
                    }
                }
                
                _ => return None,
            }
        }
    }
    
    /// 駒を取る手をスコア付け（MVV-LVA）
    fn score_captures(&mut self) {
        for scored_move in &mut self.moves {
            let move_ = scored_move.move_;
            let captured = self.pos.piece_on(move_.to()).unwrap();
            let attacker = self.pos.piece_on(move_.from()).unwrap();
            
            // Most Valuable Victim - Least Valuable Attacker
            scored_move.score = captured.value() * 10 - attacker.value();
            
            // 成りボーナス
            if move_.is_promote() {
                scored_move.score += 500;
            }
        }
    }
    
    /// 静かな手をスコア付け
    fn score_quiets(&mut self) {
        for scored_move in &mut self.moves {
            let move_ = scored_move.move_;
            scored_move.score = self.history.get(&self.pos, move_);
        }
    }
    
    /// 最高スコアの手を選択
    fn pick_best(&mut self) -> Option<Move> {
        if self.current >= self.moves.len() {
            return None;
        }
        
        // 残りの中で最高スコアを見つける
        let best_idx = self.current + 
            self.moves[self.current..]
                .iter()
                .enumerate()
                .max_by_key(|(_, m)| m.score)
                .map(|(i, _)| i)?;
        
        self.moves.swap(self.current, best_idx);
        let result = self.moves[self.current].move_;
        self.current += 1;
        
        Some(result)
    }
}
```

## 4. 並列探索（Lazy SMP）

### 4.1 スレッドプール

```rust
use std::thread;
use crossbeam::channel::{bounded, Sender, Receiver};

/// 探索スレッド
pub struct SearchThread {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
    control: ThreadControl,
}

/// スレッド制御用チャンネル
struct ThreadControl {
    start_tx: Sender<SearchCommand>,
    result_rx: Receiver<SearchResult>,
}

/// 探索コマンド
enum SearchCommand {
    Search {
        pos: Position,
        limits: SearchLimits,
        depth_offset: i32,
    },
    Stop,
    Quit,
}

/// 探索結果
struct SearchResult {
    best_move: Move,
    score: Value,
    depth: Depth,
    nodes: u64,
    pv: Vec<Move>,
}

/// スレッドプール
pub struct ThreadPool {
    threads: Vec<SearchThread>,
    tt: Arc<TranspositionTable>,
    main_thread_id: usize,
}

impl ThreadPool {
    pub fn new(num_threads: usize, tt_size_mb: usize) -> Self {
        let tt = Arc::new(TranspositionTable::new(tt_size_mb));
        let mut threads = Vec::new();
        
        for id in 0..num_threads {
            let (start_tx, start_rx) = bounded(1);
            let (result_tx, result_rx) = bounded(1);
            
            let tt_clone = Arc::clone(&tt);
            let thread = thread::spawn(move || {
                let mut worker = SearchWorker::new(id, tt_clone);
                worker.run(start_rx, result_tx);
            });
            
            threads.push(SearchThread {
                id,
                thread: Some(thread),
                control: ThreadControl { start_tx, result_rx },
            });
        }
        
        ThreadPool {
            threads,
            tt,
            main_thread_id: 0,
        }
    }
    
    /// 並列探索を開始
    pub fn start_search(&mut self, pos: &Position, limits: SearchLimits) -> SearchResult {
        // 各スレッドに探索開始を指示
        for (i, thread) in self.threads.iter().enumerate() {
            let depth_offset = if i == self.main_thread_id { 0 } else { i as i32 % 4 };
            
            thread.control.start_tx.send(SearchCommand::Search {
                pos: pos.clone(),
                limits: limits.clone(),
                depth_offset,
            }).unwrap();
        }
        
        // 結果を収集
        let mut best_result = None;
        let mut total_nodes = 0;
        
        for thread in &self.threads {
            if let Ok(result) = thread.control.result_rx.recv() {
                total_nodes += result.nodes;
                
                if thread.id == self.main_thread_id || best_result.is_none() {
                    best_result = Some(result);
                }
            }
        }
        
        let mut result = best_result.unwrap();
        result.nodes = total_nodes;
        result
    }
}
```

### 4.2 探索ワーカー

```rust
/// 探索ワーカー
struct SearchWorker {
    id: usize,
    tt: Arc<TranspositionTable>,
    searcher: EnhancedSearcher,
}

impl SearchWorker {
    fn new(id: usize, tt: Arc<TranspositionTable>) -> Self {
        SearchWorker {
            id,
            tt: Arc::clone(&tt),
            searcher: EnhancedSearcher::new(tt),
        }
    }
    
    fn run(
        &mut self,
        start_rx: Receiver<SearchCommand>,
        result_tx: Sender<SearchResult>,
    ) {
        loop {
            match start_rx.recv() {
                Ok(SearchCommand::Search { pos, limits, depth_offset }) => {
                    let result = self.iterative_deepening(&pos, limits, depth_offset);
                    result_tx.send(result).ok();
                }
                Ok(SearchCommand::Stop) => {
                    self.searcher.stop();
                }
                Ok(SearchCommand::Quit) | Err(_) => {
                    break;
                }
            }
        }
    }
    
    fn iterative_deepening(
        &mut self,
        pos: &Position,
        limits: SearchLimits,
        depth_offset: i32,
    ) -> SearchResult {
        let mut result = SearchResult {
            best_move: Move::null(),
            score: 0,
            depth: 0,
            nodes: 0,
            pv: Vec::new(),
        };
        
        // 深さを変えながら探索（Lazy SMP）
        for depth in 1..=limits.max_depth {
            // 非メインスレッドは特定の深さをスキップ
            if self.id != 0 && (depth + depth_offset) % 4 != 0 {
                continue;
            }
            
            // Aspiration Window
            let (score, pv) = self.aspiration_search(pos, result.score, depth);
            
            result.best_move = pv.get(0).copied().unwrap_or(Move::null());
            result.score = score;
            result.depth = depth;
            result.pv = pv;
            
            // 時間チェック
            if self.searcher.should_stop() {
                break;
            }
            
            // 詰みを発見
            if score.abs() > VALUE_KNOWN_WIN {
                break;
            }
        }
        
        result.nodes = self.searcher.nodes();
        result
    }
    
    fn aspiration_search(&mut self, pos: &Position, prev_score: Value, depth: Depth) -> (Value, Vec<Move>) {
        let mut alpha = -VALUE_INFINITE;
        let mut beta = VALUE_INFINITE;
        let mut delta = self.searcher.params.aspiration_window_delta;
        
        // 前回のスコアがある場合は狭い窓から開始
        if depth >= 4 && prev_score.abs() < VALUE_KNOWN_WIN {
            alpha = (prev_score - delta).max(-VALUE_INFINITE);
            beta = (prev_score + delta).min(VALUE_INFINITE);
        }
        
        loop {
            let score = self.searcher.search_root(pos, alpha, beta, depth);
            
            // 窓の外なら再探索
            if score <= alpha {
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-VALUE_INFINITE);
            } else if score >= beta {
                alpha = (alpha + beta) / 2;
                beta = (score + delta).min(VALUE_INFINITE);
            } else {
                return (score, self.searcher.get_pv());
            }
            
            delta += delta / 4 + 5;
            
            // 窓が広がりすぎたら通常探索
            if delta > 500 {
                alpha = -VALUE_INFINITE;
                beta = VALUE_INFINITE;
            }
        }
    }
}
```

## 5. 並列対応置換表

### 5.1 ロックフリー実装

```rust
use std::sync::atomic::{AtomicU64, AtomicU32};

/// 置換表エントリ（16バイト）
#[repr(C, align(16))]
pub struct TTEntry {
    data: AtomicU64,  // key(16) + move(16) + value(16) + eval(16)
    meta: AtomicU64,  // depth(8) + bound(2) + generation(6) + padding(48)
}

impl TTEntry {
    /// アトミックに読み込み
    pub fn load(&self) -> Option<TTData> {
        let data = self.data.load(Ordering::Relaxed);
        let meta = self.meta.load(Ordering::Relaxed);
        
        let key = (data >> 48) as u16;
        if key == 0 {
            return None;
        }
        
        Some(TTData {
            key,
            best_move: Move::from_u16((data >> 32) as u16),
            value: ((data >> 16) as i16) as Value,
            eval: (data as i16) as Value,
            depth: (meta >> 56) as Depth,
            bound: unsafe { std::mem::transmute((meta >> 54) as u8 & 0x3) },
            generation: (meta >> 48) as u8 & 0x3F,
        })
    }
    
    /// アトミックに書き込み
    pub fn store(&self, data: TTData) {
        let data_bits = 
            ((data.key as u64) << 48) |
            ((data.best_move.to_u16() as u64) << 32) |
            ((data.value as u16 as u64) << 16) |
            (data.eval as u16 as u64);
        
        let meta_bits =
            ((data.depth as u64) << 56) |
            ((data.bound as u64) << 54) |
            ((data.generation as u64) << 48);
        
        self.data.store(data_bits, Ordering::Relaxed);
        self.meta.store(meta_bits, Ordering::Relaxed);
    }
}

/// 置換表
pub struct TranspositionTable {
    table: Vec<TTCluster>,
    size_mask: usize,
    generation: AtomicU8,
}

/// TTクラスター（4エントリ）
#[repr(C, align(64))]
struct TTCluster {
    entries: [TTEntry; 4],
}

impl TranspositionTable {
    pub fn new(size_mb: usize) -> Self {
        let size = (size_mb * 1024 * 1024) / std::mem::size_of::<TTCluster>();
        let size = size.next_power_of_two();
        
        let mut table = Vec::with_capacity(size);
        for _ in 0..size {
            table.push(TTCluster {
                entries: unsafe { std::mem::zeroed() },
            });
        }
        
        TranspositionTable {
            table,
            size_mask: size - 1,
            generation: AtomicU8::new(0),
        }
    }
    
    /// エントリを検索
    pub fn probe(&self, hash: u64) -> Option<TTData> {
        let key = (hash >> 48) as u16;
        let index = (hash as usize) & self.size_mask;
        let cluster = &self.table[index];
        
        // 4つのエントリを確認
        for entry in &cluster.entries {
            if let Some(data) = entry.load() {
                if data.key == key {
                    return Some(data);
                }
            }
        }
        
        None
    }
    
    /// エントリを保存
    pub fn store(&self, hash: u64, move_: Move, value: Value, eval: Value, depth: Depth, bound: Bound) {
        let key = (hash >> 48) as u16;
        let index = (hash as usize) & self.size_mask;
        let cluster = &self.table[index];
        let generation = self.generation.load(Ordering::Relaxed);
        
        let new_data = TTData {
            key,
            best_move: move_,
            value,
            eval,
            depth,
            bound,
            generation,
        };
        
        // 置き換え戦略
        let mut replace_idx = 0;
        let mut min_score = i32::MAX;
        
        for (i, entry) in cluster.entries.iter().enumerate() {
            if let Some(data) = entry.load() {
                if data.key == key {
                    // 同じ局面が見つかった
                    if data.depth < depth || data.generation != generation {
                        cluster.entries[i].store(new_data);
                    }
                    return;
                }
                
                // 置き換えスコア計算
                let score = (data.generation == generation) as i32 * 256 + data.depth as i32;
                if score < min_score {
                    min_score = score;
                    replace_idx = i;
                }
            } else {
                // 空きエントリ
                cluster.entries[i].store(new_data);
                return;
            }
        }
        
        // 最も価値の低いエントリを置き換え
        cluster.entries[replace_idx].store(new_data);
    }
    
    /// 新しい探索のために世代を更新
    pub fn new_search(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
    
    /// 使用率を取得
    pub fn hashfull(&self) -> u32 {
        let sample_size = 1000;
        let mut used = 0;
        
        for i in 0..sample_size {
            let cluster = &self.table[i & self.size_mask];
            for entry in &cluster.entries {
                if entry.load().is_some() {
                    used += 1;
                }
            }
        }
        
        (used * 1000) / (sample_size * 4)
    }
}
```

## 6. 時間管理

### 6.1 動的時間配分

```rust
use std::time::{Duration, Instant};

/// 時間管理
pub struct TimeManager {
    start_time: Instant,
    time_limit: Duration,
    max_time: Duration,
    move_overhead: Duration,
    stability_factor: f64,
}

/// 探索制限
#[derive(Clone)]
pub struct SearchLimits {
    pub time: [Option<Duration>; 2],      // 各手番の残り時間
    pub inc: [Option<Duration>; 2],       // 各手番の増加時間
    pub moves_to_go: Option<u32>,         // 次の時間制御までの手数
    pub depth: Option<Depth>,             // 最大深さ
    pub nodes: Option<u64>,               // 最大ノード数
    pub mate: Option<u32>,                // 詰み探索の手数
    pub movetime: Option<Duration>,       // 固定思考時間
    pub infinite: bool,                   // 無限探索
}

impl TimeManager {
    pub fn new() -> Self {
        TimeManager {
            start_time: Instant::now(),
            time_limit: Duration::from_secs(0),
            max_time: Duration::from_secs(0),
            move_overhead: Duration::from_millis(50),
            stability_factor: 1.0,
        }
    }
    
    /// 探索開始時の時間配分
    pub fn init(&mut self, limits: &SearchLimits, color: Color, ply: u32) {
        self.start_time = Instant::now();
        
        if limits.infinite {
            self.time_limit = Duration::from_secs(86400); // 24時間
            self.max_time = self.time_limit;
            return;
        }
        
        if let Some(movetime) = limits.movetime {
            self.time_limit = movetime;
            self.max_time = movetime;
            return;
        }
        
        let our_time = limits.time[color as usize].unwrap_or(Duration::from_secs(0));
        let our_inc = limits.inc[color as usize].unwrap_or(Duration::from_secs(0));
        
        if our_time.is_zero() {
            self.time_limit = Duration::from_secs(0);
            self.max_time = Duration::from_secs(0);
            return;
        }
        
        // 基本的な時間配分
        let moves_left = limits.moves_to_go.unwrap_or(50).max(1) as f64;
        let time_left = our_time.as_millis() as f64;
        let inc = our_inc.as_millis() as f64;
        
        // 時間配分の計算
        let base_time = if ply < 20 {
            // 序盤は少し多めに
            (time_left / moves_left + inc * 0.8) * 1.2
        } else if ply > 60 {
            // 終盤は慎重に
            (time_left / moves_left + inc * 0.9) * 0.8
        } else {
            // 中盤は標準
            time_left / moves_left + inc * 0.85
        };
        
        // オーバーヘッドを考慮
        let overhead = self.move_overhead.as_millis() as f64;
        let time_limit = (base_time - overhead).max(10.0);
        let max_time = (time_left * 0.8 - overhead).min(time_limit * 6.0).max(time_limit);
        
        self.time_limit = Duration::from_millis(time_limit as u64);
        self.max_time = Duration::from_millis(max_time as u64);
    }
    
    /// 探索を停止すべきか判定
    pub fn check_time(&self) -> bool {
        if self.time_limit.is_zero() {
            return false;
        }
        
        let elapsed = self.start_time.elapsed();
        elapsed >= self.time_limit
    }
    
    /// 安定性に基づく時間調整
    pub fn adjust_time(&mut self, best_move_changes: u32, depth: Depth) {
        if depth < 4 {
            return;
        }
        
        // 最善手の変更回数に基づく調整
        self.stability_factor = match best_move_changes {
            0..=2 => 0.8,   // 安定している
            3..=5 => 1.0,   // 通常
            6..=9 => 1.2,   // やや不安定
            _ => 1.5,       // 非常に不安定
        };
        
        let new_limit = Duration::from_millis(
            (self.time_limit.as_millis() as f64 * self.stability_factor) as u64
        );
        
        self.time_limit = new_limit.min(self.max_time);
    }
}
```

## 7. テスト計画

### 7.1 探索アルゴリズムテスト

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    /// テスト用の局面
    fn test_positions() -> Vec<(&'static str, &'static str, i32)> {
        vec![
            // SFEN, 最善手, 期待される評価値の符号
            ("startpos", "7g7f", 0),
            ("4k4/9/4P4/9/9/9/9/9/4K4 b - 1", "5f5e", 1), // 歩得
            ("4k4/9/9/9/4p4/9/9/9/4K4 w - 1", "5e5f", -1), // 歩損
        ]
    }
    
    #[test]
    fn test_search_finds_best_move() {
        let tt = Arc::new(TranspositionTable::new(16));
        let mut searcher = EnhancedSearcher::new(Arc::clone(&tt));
        
        for (sfen, expected_move, eval_sign) in test_positions() {
            let pos = Position::from_sfen(sfen).unwrap();
            let limits = SearchLimits {
                depth: Some(10),
                ..Default::default()
            };
            
            let result = searcher.iterative_deepening(&pos, limits, 0);
            
            assert_eq!(
                result.best_move.to_string(),
                expected_move,
                "Position: {}", sfen
            );
            
            assert!(
                result.score.signum() == eval_sign || eval_sign == 0,
                "Position: {}, Score: {}", sfen, result.score
            );
        }
    }
    
    #[test]
    fn test_null_move_pruning() {
        let tt = Arc::new(TranspositionTable::new(16));
        let mut searcher = EnhancedSearcher::new(Arc::clone(&tt));
        
        // Null moveが効果的な局面
        let pos = Position::from_sfen("4k4/9/9/9/9/9/PPP6/1B5R1/L1SGK2NL b - 1").unwrap();
        
        // Null moveなし
        searcher.params.null_move_enabled = false;
        let result1 = searcher.search(&pos, -10000, 10000, 8, false);
        let nodes1 = searcher.nodes();
        
        // Null moveあり
        searcher.reset_stats();
        searcher.params.null_move_enabled = true;
        let result2 = searcher.search(&pos, -10000, 10000, 8, false);
        let nodes2 = searcher.nodes();
        
        // 同じ評価値で、ノード数が減少
        assert_eq!(result1, result2);
        assert!(nodes2 < nodes1 * 0.8, "Null move should reduce nodes");
    }
}
```

### 7.2 並列性能テスト

```rust
#[cfg(test)]
mod parallel_tests {
    use super::*;
    
    #[test]
    fn test_lazy_smp_speedup() {
        let pos = Position::from_sfen("startpos moves 7g7f 3c3d").unwrap();
        let limits = SearchLimits {
            depth: Some(12),
            ..Default::default()
        };
        
        // シングルスレッド
        let start = Instant::now();
        let mut pool1 = ThreadPool::new(1, 128);
        let result1 = pool1.start_search(&pos, limits.clone());
        let time1 = start.elapsed();
        
        // 4スレッド
        let start = Instant::now();
        let mut pool4 = ThreadPool::new(4, 128);
        let result4 = pool4.start_search(&pos, limits.clone());
        let time4 = start.elapsed();
        
        // 同じ結果
        assert_eq!(result1.best_move, result4.best_move);
        assert!((result1.score - result4.score).abs() < 50);
        
        // スピードアップ
        let speedup = time1.as_millis() as f64 / time4.as_millis() as f64;
        assert!(speedup > 2.0, "4 threads should be >2x faster");
    }
    
    #[test]
    fn test_tt_concurrent_access() {
        use std::sync::Arc;
        use std::thread;
        
        let tt = Arc::new(TranspositionTable::new(16));
        let mut handles = vec![];
        
        // 複数スレッドから同時アクセス
        for i in 0..8 {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                for j in 0..10000 {
                    let hash = ((i * 10000 + j) as u64).wrapping_mul(0x9e3779b97f4a7c15);
                    
                    // 書き込み
                    tt_clone.store(
                        hash,
                        Move::null(),
                        j as Value,
                        0,
                        10,
                        Bound::Exact
                    );
                    
                    // 読み込み
                    if let Some(data) = tt_clone.probe(hash) {
                        assert_eq!(data.value, j as Value);
                    }
                }
            });
            handles.push(handle);
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        // 使用率チェック
        let hashfull = tt.hashfull();
        assert!(hashfull > 0, "TT should contain entries");
    }
}
```

## 8. 実装スケジュール

### Week 1: 探索強化
- Day 1: PVS探索の実装
- Day 2: Null Move PruningとLMR
- Day 3: Singular ExtensionとProbCut  
- Day 4: 履歴ヒューリスティック
- Day 5: 手の順序付け最適化
- Day 6-7: テストとデバッグ

### Week 2: 並列化と最適化
- Day 1-2: ロックフリー置換表
- Day 3-4: Lazy SMP実装
- Day 5: 時間管理システム
- Day 6: 性能チューニング
- Day 7: 統合テスト

## 9. 成功基準

### 機能要件
- [ ] 主要な枝刈り技術の実装
- [ ] 16スレッドまでのスケーラビリティ
- [ ] 正確な時間管理
- [ ] 置換表の並列安全性

### 性能要件
- [ ] 探索速度: 150-200万NPS（4スレッド）
- [ ] 並列効率: 4スレッドで2.5倍以上
- [ ] 探索深度: 初期局面で15手以上/秒
- [ ] 置換表衝突率: 2^-32以下（u32キー使用）

### 品質要件
- [ ] 各技術の効果測定
- [ ] 並列性のストレステスト
- [ ] 時間管理の精度

## 10. リスクと対策

### 技術的リスク
1. **並列化のバグ**
   - 対策: Thread Sanitizerの使用
   - 徹底的な並列テスト

2. **探索の不安定性**
   - 対策: 各技術の段階的導入
   - パラメータの慎重な調整

3. **時間管理の精度**
   - 対策: 実戦的なテスト
   - 安全マージンの確保

## 11. Phase 4への準備

Phase 3完了時に、以下が準備されています：

1. **高度な探索エンジン**: プロレベルの探索深度
2. **並列処理基盤**: マルチコアCPUの活用
3. **時間管理**: 実戦での使用準備
4. **最適化された実装**: 本番環境対応

これらにより、Phase 4でのWASM統合と最終調整が可能になります。