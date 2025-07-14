# Phase 1: 基盤実装 - 詳細設計書

> **親ドキュメント**: [Rust将棋AI実装要件書](./rust-shogi-ai-requirements.md)  
> **該当セクション**: 9. 開発マイルストーン - Phase 1: 基盤実装（3週間）

## 1. 概要

Phase 1では、将棋AIエンジンの基盤となるデータ構造と基本的な探索機能を実装します。この段階では、高度な最適化よりも正確性と拡張性を重視します。

### 1.1 目標
- ビットボードによる高速な盤面表現
- 完全に正確な合法手生成
- 基本的なαβ探索の実装
- 簡易評価関数による局面評価

### 1.2 成果物
- `board.rs`: ビットボード実装
- `movegen.rs`: 合法手生成
- `search.rs`: 基本探索エンジン
- `evaluate.rs`: 簡易評価関数
- 単体テストスイート

## 2. ビットボード実装（board.rs）

### 2.1 データ構造設計

```rust
use std::fmt;

/// 将棋盤の座標を表す型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Square(u8); // 0-80 (9x9)

impl Square {
    pub const fn new(file: u8, rank: u8) -> Self {
        debug_assert!(file < 9 && rank < 9);
        Square(rank * 9 + file)
    }
    
    pub const fn file(self) -> u8 { self.0 % 9 }
    pub const fn rank(self) -> u8 { self.0 / 9 }
    pub const fn index(self) -> usize { self.0 as usize }
}

/// 駒の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceType {
    King = 0,
    Rook = 1,
    Bishop = 2,
    Gold = 3,
    Silver = 4,
    Knight = 5,
    Lance = 6,
    Pawn = 7,
}

/// 成り駒を含む駒の完全な表現
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    piece_type: PieceType,
    color: Color,
    promoted: bool,
}

/// 手番
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Black = 0, // 先手
    White = 1, // 後手
}

/// ビットボード（81マス対応）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bitboard(u128); // 下位81ビットを使用

impl Bitboard {
    pub const EMPTY: Self = Bitboard(0);
    pub const ALL: Self = Bitboard((1u128 << 81) - 1);
    
    pub fn set(&mut self, sq: Square) {
        self.0 |= 1u128 << sq.index();
    }
    
    pub fn clear(&mut self, sq: Square) {
        self.0 &= !(1u128 << sq.index());
    }
    
    pub fn test(&self, sq: Square) -> bool {
        (self.0 >> sq.index()) & 1 != 0
    }
    
    pub fn pop_lsb(&mut self) -> Option<Square> {
        if self.0 == 0 {
            return None;
        }
        let lsb = self.0.trailing_zeros() as u8;
        self.0 &= self.0 - 1; // Clear LSB
        Some(Square(lsb))
    }
}

/// 局面を表す構造体
pub struct Position {
    // 駒種別・色別のビットボード
    piece_bb: [[Bitboard; 8]; 2], // [color][piece_type] - 8駒種対応
    
    // 全ての駒のビットボード（高速化用キャッシュ）
    occupied_bb: [Bitboard; 2], // [color]
    all_bb: Bitboard,
    
    // 持ち駒
    hands: [[u8; 7]; 2], // [color][piece_type] (Kingを除く)
    
    // ゲーム状態
    side_to_move: Color,
    ply: u16,
    
    // 千日手検出用の履歴
    history: Vec<u64>, // Zobrist hash
}
```

### 2.2 Zobristハッシュ

```rust
/// Zobristハッシュ用の乱数テーブル
pub struct ZobristTable {
    piece_square: [[[u64; 81]; 14]; 2], // [color][piece_kind][square]
    hand: [[u64; 19]; 7], // [piece_type][count] (最大18枚)
    side: u64, // 手番
}

impl ZobristTable {
    pub fn new() -> Self {
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;
        
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1234567890ABCDEF);
        
        let mut table = ZobristTable {
            piece_square: [[[0; 81]; 14]; 2],
            hand: [[0; 19]; 7],
            side: rng.gen(),
        };
        
        // 乱数生成
        for color in 0..2 {
            for piece in 0..14 {
                for sq in 0..81 {
                    table.piece_square[color][piece][sq] = rng.gen();
                }
            }
        }
        
        for piece in 0..7 {
            for count in 0..19 {
                table.hand[piece][count] = rng.gen();
            }
        }
        
        table
    }
}

lazy_static! {
    pub static ref ZOBRIST: ZobristTable = ZobristTable::new();
}

impl Position {
    pub fn hash(&self) -> u64 {
        let mut hash = 0u64;
        
        // 盤上の駒
        for color in 0..2 {
            for piece_type in 0..8 {
                let mut bb = self.piece_bb[color][piece_type];
                while let Some(sq) = bb.pop_lsb() {
                    hash ^= ZOBRIST.piece_square[color][piece_type][sq.index()];
                }
            }
        }
        
        // 持ち駒
        for color in 0..2 {
            for piece_type in 0..7 {
                let count = self.hands[color][piece_type] as usize;
                if count > 0 {
                    hash ^= ZOBRIST.hand[piece_type][count];
                }
            }
        }
        
        // 手番
        if self.side_to_move == Color::White {
            hash ^= ZOBRIST.side;
        }
        
        hash
    }
}
```

### 2.3 基本操作

```rust
impl Position {
    /// 初期局面を生成
    pub fn startpos() -> Self {
        let mut pos = Position {
            piece_bb: [[Bitboard::EMPTY; 8]; 2],
            occupied_bb: [Bitboard::EMPTY; 2],
            all_bb: Bitboard::EMPTY,
            hands: [[0; 7]; 2],
            side_to_move: Color::Black,
            ply: 0,
            history: Vec::new(),
        };
        
        // 初期配置の設定
        // 先手の駒
        pos.put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.put_piece(Square::new(3, 8), Piece::new(PieceType::Gold, Color::Black));
        pos.put_piece(Square::new(5, 8), Piece::new(PieceType::Gold, Color::Black));
        // ... 省略 ...
        
        pos.update_occupied();
        pos
    }
    
    /// 駒を配置
    fn put_piece(&mut self, sq: Square, piece: Piece) {
        let color = piece.color as usize;
        let piece_type = piece.piece_type as usize;
        self.piece_bb[color][piece_type].set(sq);
    }
    
    /// 駒を取り除く
    fn remove_piece(&mut self, sq: Square) -> Option<Piece> {
        for color in 0..2 {
            for piece_type in 0..8 {
                if self.piece_bb[color][piece_type].test(sq) {
                    self.piece_bb[color][piece_type].clear(sq);
                    return Some(Piece::new(
                        unsafe { std::mem::transmute(piece_type as u8) },
                        unsafe { std::mem::transmute(color as u8) },
                    ));
                }
            }
        }
        None
    }
    
    /// occupiedビットボードを更新
    fn update_occupied(&mut self) {
        self.occupied_bb[0] = Bitboard::EMPTY;
        self.occupied_bb[1] = Bitboard::EMPTY;
        
        for piece_type in 0..8 {
            self.occupied_bb[0].0 |= self.piece_bb[0][piece_type].0;
            self.occupied_bb[1].0 |= self.piece_bb[1][piece_type].0;
        }
        
        self.all_bb.0 = self.occupied_bb[0].0 | self.occupied_bb[1].0;
    }
}
```

## 3. 合法手生成（movegen.rs）

### 3.1 移動の表現

```rust
/// 移動を表す構造体
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    data: u16, // Compact representation
}

impl Move {
    /// 盤上の駒を動かす手
    pub fn normal(from: Square, to: Square, promote: bool) -> Self {
        let mut data = 0u16;
        data |= from.0 as u16; // bits 0-6
        data |= (to.0 as u16) << 7; // bits 7-13
        if promote {
            data |= 1 << 14; // bit 14
        }
        Move { data }
    }
    
    /// 持ち駒を打つ手
    pub fn drop(piece_type: PieceType, to: Square) -> Self {
        let mut data = 0u16;
        data |= 1 << 15; // Drop flag
        data |= (piece_type as u16) << 11; // bits 11-13
        data |= (to.0 as u16) << 7; // bits 7-10
        Move { data }
    }
    
    pub fn is_drop(self) -> bool { self.data & (1 << 15) != 0 }
    pub fn from(self) -> Square { Square((self.data & 0x7F) as u8) }
    pub fn to(self) -> Square { Square(((self.data >> 7) & 0x7F) as u8) }
    pub fn is_promote(self) -> bool { self.data & (1 << 14) != 0 }
    pub fn drop_piece_type(self) -> PieceType {
        unsafe { std::mem::transmute(((self.data >> 11) & 0x7) as u8) }
    }
}
```

### 3.2 Magic Bitboard

```rust
/// Magic Bitboardのエントリ
pub struct MagicEntry {
    mask: Bitboard,
    magic: u128,
    shift: u8,
    offset: usize,
}

/// 飛車・角の利きテーブル
pub struct AttackTables {
    rook_magics: [MagicEntry; 81],
    bishop_magics: [MagicEntry; 81],
    attacks: Vec<Bitboard>, // 共有テーブル
}

impl AttackTables {
    pub fn new() -> Self {
        // 事前計算されたMagic定数とテーブルを初期化
        // 実装は長いので省略
        todo!()
    }
    
    pub fn rook_attacks(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        let entry = &self.rook_magics[sq.index()];
        let index = (((occupied.0 & entry.mask.0).wrapping_mul(entry.magic)) 
                    >> entry.shift) as usize;
        self.attacks[entry.offset + index]
    }
    
    pub fn bishop_attacks(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        let entry = &self.bishop_magics[sq.index()];
        let index = (((occupied.0 & entry.mask.0).wrapping_mul(entry.magic)) 
                    >> entry.shift) as usize;
        self.attacks[entry.offset + index]
    }
}

lazy_static! {
    pub static ref ATTACK_TABLES: AttackTables = AttackTables::new();
}
```

### 3.3 合法手生成

```rust
/// 合法手生成器
pub struct MoveGen<'a> {
    pos: &'a Position,
    moves: Vec<Move>,
}

impl<'a> MoveGen<'a> {
    pub fn new(pos: &'a Position) -> Self {
        MoveGen { pos, moves: Vec::with_capacity(256) }
    }
    
    /// 全ての合法手を生成
    pub fn generate_all(&mut self) -> &[Move] {
        self.generate_captures();
        self.generate_non_captures();
        self.generate_drops();
        
        // 自玉が王手されているかチェック
        if self.is_in_check() {
            self.filter_legal_moves();
        }
        
        &self.moves
    }
    
    /// 駒を取る手の生成
    fn generate_captures(&mut self) {
        let us = self.pos.side_to_move;
        let them = us.opposite();
        let target = self.pos.occupied_bb[them as usize];
        
        // 各駒種について
        for piece_type in 0..8 {
            let mut pieces = self.pos.piece_bb[us as usize][piece_type];
            while let Some(from) = pieces.pop_lsb() {
                let attacks = self.attacks_from(from, piece_type);
                let captures = attacks.0 & target.0;
                self.add_moves(from, Bitboard(captures), piece_type);
            }
        }
    }
    
    /// 駒を取らない手の生成
    fn generate_non_captures(&mut self) {
        let us = self.pos.side_to_move;
        let target = !self.pos.all_bb; // 空いているマス
        
        for piece_type in 0..8 {
            let mut pieces = self.pos.piece_bb[us as usize][piece_type];
            while let Some(from) = pieces.pop_lsb() {
                let attacks = self.attacks_from(from, piece_type);
                let non_captures = attacks.0 & target.0;
                self.add_moves(from, Bitboard(non_captures), piece_type);
            }
        }
    }
    
    /// 持ち駒を打つ手の生成
    fn generate_drops(&mut self) {
        let us = self.pos.side_to_move;
        let empty = !self.pos.all_bb;
        
        for piece_type in 0..7 { // Kingを除く
            let count = self.pos.hands[us as usize][piece_type];
            if count > 0 {
                // 二歩チェック
                if piece_type == PieceType::Pawn as usize {
                    let pawn_files = self.get_pawn_files(us);
                    let legal_drops = empty.0 & !pawn_files.0;
                    self.add_drops(piece_type, Bitboard(legal_drops));
                } else {
                    self.add_drops(piece_type, empty);
                }
            }
        }
    }
    
    /// 指定した駒の利きを計算
    fn attacks_from(&self, from: Square, piece_type: usize) -> Bitboard {
        match piece_type {
            0 => self.king_attacks(from),
            1 => ATTACK_TABLES.rook_attacks(from, self.pos.all_bb),
            2 => ATTACK_TABLES.bishop_attacks(from, self.pos.all_bb),
            3 => self.gold_attacks(from),
            4 => self.silver_attacks(from),
            5 => self.knight_attacks(from),
            6 => self.lance_attacks(from),
            7 => self.pawn_attacks(from),
            _ => unreachable!(),
        }
    }
    
    /// 王手されているかチェック
    fn is_in_check(&self) -> bool {
        let us = self.pos.side_to_move;
        let king_sq = self.pos.king_square(us);
        self.is_attacked(king_sq, us.opposite())
    }
    
    /// 指定マスが攻撃されているかチェック
    fn is_attacked(&self, sq: Square, by: Color) -> bool {
        // 各駒種からの攻撃をチェック
        // 実装省略
        false
    }
}
```

## 4. 基本探索エンジン（search.rs）

### 4.1 探索構造体

```rust
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// 探索の統計情報
pub struct SearchInfo {
    pub nodes: AtomicU64,
    pub start_time: Instant,
    pub stop: AtomicBool,
}

/// 探索エンジン
pub struct Searcher {
    info: SearchInfo,
    history: Vec<Move>, // 探索履歴
}

/// 探索結果
#[derive(Debug)]
pub struct SearchResult {
    pub best_move: Move,
    pub score: i32,
    pub depth: i32,
    pub nodes: u64,
    pub time_ms: u64,
    pub pv: Vec<Move>, // Principal Variation
}

impl Searcher {
    pub fn new() -> Self {
        Searcher {
            info: SearchInfo {
                nodes: AtomicU64::new(0),
                start_time: Instant::now(),
                stop: AtomicBool::new(false),
            },
            history: Vec::new(),
        }
    }
    
    /// 反復深化探索
    pub fn search(&mut self, pos: &Position, time_limit_ms: u64) -> SearchResult {
        let mut result = SearchResult {
            best_move: Move::null(),
            score: 0,
            depth: 0,
            nodes: 0,
            time_ms: 0,
            pv: Vec::new(),
        };
        
        // 深さ1から順に探索
        for depth in 1..=64 {
            let score = self.alpha_beta(pos, -30000, 30000, depth, &mut result.pv);
            
            // 時間切れチェック
            let elapsed = self.info.start_time.elapsed().as_millis() as u64;
            if elapsed >= time_limit_ms {
                break;
            }
            
            result.score = score;
            result.depth = depth;
            result.best_move = result.pv.get(0).copied().unwrap_or(Move::null());
            
            // 詰みを発見したら探索終了
            if score.abs() > 20000 {
                break;
            }
        }
        
        result.nodes = self.info.nodes.load(Ordering::Relaxed);
        result.time_ms = self.info.start_time.elapsed().as_millis() as u64;
        result
    }
    
    /// アルファベータ探索
    fn alpha_beta(
        &mut self,
        pos: &Position,
        mut alpha: i32,
        beta: i32,
        depth: i32,
        pv: &mut Vec<Move>,
    ) -> i32 {
        // ノード数カウント
        self.info.nodes.fetch_add(1, Ordering::Relaxed);
        
        // 探索停止チェック
        if self.info.stop.load(Ordering::Relaxed) {
            return 0;
        }
        
        // 深さ0に達したら評価関数を呼ぶ
        if depth <= 0 {
            return self.quiescence_search(pos, alpha, beta);
        }
        
        // 合法手生成
        let mut movegen = MoveGen::new(pos);
        let moves = movegen.generate_all();
        
        // 合法手がない場合（詰みまたはステイルメイト）
        if moves.is_empty() {
            if movegen.is_in_check() {
                return -30000 + pos.ply as i32; // 詰み
            } else {
                return 0; // ステイルメイト（将棋では起こらない）
            }
        }
        
        let mut best_score = -30001;
        let mut best_move = Move::null();
        let mut child_pv = Vec::new();
        
        for &mv in moves {
            let mut new_pos = pos.clone();
            new_pos.do_move(mv);
            
            let score = -self.alpha_beta(&new_pos, -beta, -alpha, depth - 1, &mut child_pv);
            
            if score > best_score {
                best_score = score;
                best_move = mv;
                
                // PV更新
                pv.clear();
                pv.push(mv);
                pv.extend_from_slice(&child_pv);
                
                if score > alpha {
                    alpha = score;
                    if alpha >= beta {
                        break; // ベータカット
                    }
                }
            }
        }
        
        best_score
    }
    
    /// 静止探索（駒の取り合いのみ探索）
    fn quiescence_search(&mut self, pos: &Position, mut alpha: i32, beta: i32) -> i32 {
        // 現局面の評価
        let stand_pat = evaluate(pos);
        
        if stand_pat >= beta {
            return beta;
        }
        
        if stand_pat > alpha {
            alpha = stand_pat;
        }
        
        // 駒を取る手のみ生成
        let mut movegen = MoveGen::new(pos);
        let moves = movegen.generate_captures_only();
        
        for &mv in moves {
            let mut new_pos = pos.clone();
            new_pos.do_move(mv);
            
            let score = -self.quiescence_search(&new_pos, -beta, -alpha);
            
            if score >= beta {
                return beta;
            }
            
            if score > alpha {
                alpha = score;
            }
        }
        
        alpha
    }
}
```

### 4.2 手の実行

```rust
impl Position {
    /// 手を実行する
    pub fn do_move(&mut self, mv: Move) {
        self.history.push(self.hash());
        
        if mv.is_drop() {
            // 駒打ち
            let piece_type = mv.drop_piece_type();
            let to = mv.to();
            let piece = Piece::new(piece_type, self.side_to_move);
            
            self.put_piece(to, piece);
            self.hands[self.side_to_move as usize][piece_type as usize] -= 1;
        } else {
            // 駒の移動
            let from = mv.from();
            let to = mv.to();
            
            // 移動元の駒を取得
            let piece = self.remove_piece(from).unwrap();
            
            // 移動先に駒がある場合は取る
            if let Some(captured) = self.remove_piece(to) {
                let captured_type = if captured.promoted {
                    captured.piece_type.unpromoted()
                } else {
                    captured.piece_type
                };
                self.hands[self.side_to_move as usize][captured_type as usize] += 1;
            }
            
            // 成りの処理
            let new_piece = if mv.is_promote() {
                piece.promote()
            } else {
                piece
            };
            
            self.put_piece(to, new_piece);
        }
        
        // 手番交代
        self.side_to_move = self.side_to_move.opposite();
        self.ply += 1;
        self.update_occupied();
    }
}
```

## 5. 簡易評価関数（evaluate.rs）

### 5.1 駒の価値

```rust
/// 駒の基本価値
const PIECE_VALUES: [i32; 15] = [
    0,      // なし
    15000,  // 玉
    1100,   // 飛
    950,    // 角
    600,    // 金
    550,    // 銀
    450,    // 桂
    350,    // 香
    100,    // 歩
    1500,   // 龍
    1300,   // 馬
    600,    // 成銀（金と同じ）
    600,    // 成桂
    600,    // 成香
    600,    // と金
];

/// 駒の位置価値（Piece Square Table）
/// 簡略化のため、歩のみ実装
const PAWN_PST: [[i32; 81]; 2] = [
    // 先手の歩
    [
        0,  0,  0,  0,  0,  0,  0,  0,  0,
        5,  5,  5,  5,  5,  5,  5,  5,  5,
        10, 10, 10, 10, 10, 10, 10, 10, 10,
        15, 15, 15, 15, 15, 15, 15, 15, 15,
        20, 20, 20, 20, 20, 20, 20, 20, 20,
        25, 25, 25, 25, 25, 25, 25, 25, 25,
        30, 30, 30, 30, 30, 30, 30, 30, 30,
        35, 35, 35, 35, 35, 35, 35, 35, 35,
        40, 40, 40, 40, 40, 40, 40, 40, 40,
    ],
    // 後手の歩（反転）
    [
        40, 40, 40, 40, 40, 40, 40, 40, 40,
        35, 35, 35, 35, 35, 35, 35, 35, 35,
        30, 30, 30, 30, 30, 30, 30, 30, 30,
        25, 25, 25, 25, 25, 25, 25, 25, 25,
        20, 20, 20, 20, 20, 20, 20, 20, 20,
        15, 15, 15, 15, 15, 15, 15, 15, 15,
        10, 10, 10, 10, 10, 10, 10, 10, 10,
        5,  5,  5,  5,  5,  5,  5,  5,  5,
        0,  0,  0,  0,  0,  0,  0,  0,  0,
    ],
];
```

### 5.2 評価関数

```rust
/// 簡易評価関数
pub fn evaluate(pos: &Position) -> i32 {
    let mut score = 0;
    
    // 駒の価値を計算
    for color in 0..2 {
        let sign = if color == 0 { 1 } else { -1 };
        
        // 盤上の駒
        for piece_type in 0..8 {
            let mut bb = pos.piece_bb[color][piece_type];
            let count = bb.0.count_ones() as i32;
            score += sign * count * PIECE_VALUES[piece_type + 1];
            
            // 位置価値（歩のみ）
            if piece_type == PieceType::Pawn as usize {
                while let Some(sq) = bb.pop_lsb() {
                    score += sign * PAWN_PST[color][sq.index()];
                }
            }
        }
        
        // 持ち駒
        for piece_type in 0..7 {
            let count = pos.hands[color][piece_type] as i32;
            score += sign * count * PIECE_VALUES[piece_type + 1];
        }
    }
    
    // 手番の価値（先手番がやや有利）
    if pos.side_to_move == Color::Black {
        score += 10;
    } else {
        score -= 10;
    }
    
    // 先手視点のスコアを返す
    if pos.side_to_move == Color::Black {
        score
    } else {
        -score
    }
}

/// 駒の機動力を評価
pub fn mobility_score(pos: &Position) -> i32 {
    let mut score = 0;
    
    for color in 0..2 {
        let sign = if color == 0 { 1 } else { -1 };
        
        // 飛車・角の利きをカウント
        let mut rooks = pos.piece_bb[color][PieceType::Rook as usize];
        while let Some(sq) = rooks.pop_lsb() {
            let attacks = ATTACK_TABLES.rook_attacks(sq, pos.all_bb);
            score += sign * attacks.0.count_ones() as i32 * 5;
        }
        
        let mut bishops = pos.piece_bb[color][PieceType::Bishop as usize];
        while let Some(sq) = bishops.pop_lsb() {
            let attacks = ATTACK_TABLES.bishop_attacks(sq, pos.all_bb);
            score += sign * attacks.0.count_ones() as i32 * 3;
        }
    }
    
    score
}
```

## 6. テスト計画

### 6.1 単体テスト

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_bitboard_operations() {
        let mut bb = Bitboard::EMPTY;
        assert!(!bb.test(Square::new(4, 4)));
        
        bb.set(Square::new(4, 4));
        assert!(bb.test(Square::new(4, 4)));
        
        bb.clear(Square::new(4, 4));
        assert!(!bb.test(Square::new(4, 4)));
    }
    
    #[test]
    fn test_move_generation_startpos() {
        let pos = Position::startpos();
        let mut movegen = MoveGen::new(&pos);
        let moves = movegen.generate_all();
        
        // 初期局面では30手が可能
        assert_eq!(moves.len(), 30);
    }
    
    #[test]
    fn test_perft() {
        // Perftテスト（指定深さまでの合法手数を数える）
        let pos = Position::startpos();
        assert_eq!(perft(&pos, 1), 30);
        assert_eq!(perft(&pos, 2), 900);
        assert_eq!(perft(&pos, 3), 25470);
    }
    
    fn perft(pos: &Position, depth: i32) -> u64 {
        if depth == 0 {
            return 1;
        }
        
        let mut count = 0;
        let mut movegen = MoveGen::new(pos);
        let moves = movegen.generate_all();
        
        for &mv in moves {
            let mut new_pos = pos.clone();
            new_pos.do_move(mv);
            count += perft(&new_pos, depth - 1);
        }
        
        count
    }
}
```

### 6.2 ベンチマーク

```rust
#[cfg(test)]
mod bench {
    use super::*;
    use test::Bencher;
    
    #[bench]
    fn bench_move_generation(b: &mut Bencher) {
        let pos = Position::startpos();
        b.iter(|| {
            let mut movegen = MoveGen::new(&pos);
            test::black_box(movegen.generate_all());
        });
    }
    
    #[bench]
    fn bench_make_move(b: &mut Bencher) {
        let pos = Position::startpos();
        let mv = Move::normal(Square::new(7, 6), Square::new(7, 5), false);
        
        b.iter(|| {
            let mut new_pos = pos.clone();
            new_pos.do_move(mv);
            test::black_box(new_pos);
        });
    }
    
    #[bench]
    fn bench_evaluation(b: &mut Bencher) {
        let pos = Position::startpos();
        b.iter(|| {
            test::black_box(evaluate(&pos));
        });
    }
}
```

## 7. 実装スケジュール

### Week 1: データ構造とビットボード
- Day 1-2: 基本データ構造（Square, Piece, Bitboard）
- Day 3-4: Position構造体とZobristハッシュ
- Day 5: Magic Bitboardの実装
- Day 6-7: 単体テストとデバッグ

### Week 2: 合法手生成
- Day 1-2: 駒別の利き計算
- Day 3-4: 合法手生成の実装
- Day 5: 王手判定と合法性チェック
- Day 6-7: Perftテストとバグ修正

### Week 3: 探索と評価
- Day 1-2: 基本的なαβ探索
- Day 3: 静止探索
- Day 4: 簡易評価関数
- Day 5-6: 統合テスト
- Day 7: パフォーマンス測定と最適化

## 8. 成功基準

### 機能要件
- [ ] 完全に正確な合法手生成（Perftテストをパス）
- [ ] 基本的なαβ探索の動作
- [ ] 簡易評価関数による局面評価
- [ ] 千日手の検出

### 性能要件
- [ ] 合法手生成: 100万局面/秒以上
- [ ] 探索深度: 5手以上を1秒以内
- [ ] メモリ使用量: 10MB以下

### 品質要件
- [ ] 単体テストカバレッジ: 90%以上
- [ ] ベンチマークの安定性
- [ ] コードのドキュメント化

## 9. リスクと対策

### 技術的リスク
1. **Magic Bitboardの実装が複雑**
   - 対策: 既存実装を参考に、段階的に実装
   - 代替案: 初期は単純な利き計算で実装

2. **合法手生成のバグ**
   - 対策: Perftテストによる徹底的な検証
   - テストケースの充実

3. **パフォーマンス不足**
   - 対策: プロファイリングによるボトルネック特定
   - 必要に応じてunsafeコードの使用

### スケジュールリスク
1. **実装の遅延**
   - 対策: 優先順位を明確化
   - 最低限の機能から実装

2. **デバッグに時間がかかる**
   - 対策: テスト駆動開発
   - 早期からの統合テスト

## 10. 次のフェーズへの準備

Phase 1の完了時点で、以下が準備できている必要があります：

1. **安定したデータ構造**: Phase 2のNNUE実装の基盤
2. **正確な合法手生成**: 探索の信頼性の基礎
3. **基本的な探索フレームワーク**: Phase 3での拡張の土台
4. **テストインフラ**: 継続的な品質保証

これらの基盤により、Phase 2以降の高度な実装がスムーズに進められます。