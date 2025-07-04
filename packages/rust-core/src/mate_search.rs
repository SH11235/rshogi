use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use web_sys::Performance;

// 駒の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PieceType {
    King,   // 玉/王
    Rook,   // 飛車
    Bishop, // 角
    Gold,   // 金
    Silver, // 銀
    Knight, // 桂
    Lance,  // 香
    Pawn,   // 歩
}

// プレイヤー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    Black,
    White,
}

impl Player {
    fn opponent(&self) -> Player {
        match self {
            Player::Black => Player::White,
            Player::White => Player::Black,
        }
    }
}

// 駒
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub piece_type: PieceType,
    pub owner: Player,
    pub promoted: bool,
}

// マス目
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Square {
    pub row: u8,    // 1-9
    pub column: u8, // 1-9
}

impl Square {
    pub fn new(row: u8, column: u8) -> Option<Square> {
        if row >= 1 && row <= 9 && column >= 1 && column <= 9 {
            Some(Square { row, column })
        } else {
            None
        }
    }

    pub fn to_key(&self) -> String {
        format!("{}{}", self.row, self.column)
    }
}

// 手
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Normal {
        from: Square,
        to: Square,
        promote: bool,
    },
    Drop {
        piece_type: PieceType,
        to: Square,
    },
}

// 盤面
pub type Board = HashMap<Square, Piece>;

// 持ち駒
pub type Hand = HashMap<PieceType, u8>;
pub struct Hands {
    pub black: Hand,
    pub white: Hand,
}

// 詰み探索結果
#[wasm_bindgen]
pub struct MateSearchResult {
    pub is_mate: bool,
    pub move_count: usize,
    pub node_count: u32,
    pub elapsed_ms: u32,
}

// 詰み探索エンジン
pub struct MateSearchEngine {
    node_count: u32,
    start_time: f64,
    timeout_ms: u32,
    performance: Performance,
}

impl MateSearchEngine {
    pub fn new(timeout_ms: u32) -> Self {
        let window = web_sys::window().expect("window should exist");
        let performance = window.performance().expect("performance should exist");

        MateSearchEngine {
            node_count: 0,
            start_time: performance.now(),
            timeout_ms,
            performance,
        }
    }

    // 詰み探索メイン関数
    pub fn search(
        &mut self,
        board: &Board,
        hands: &Hands,
        attacker: Player,
        max_depth: u8,
    ) -> (bool, Vec<Move>) {
        self.node_count = 0;
        self.start_time = self.performance.now();

        // 奇数深さで探索（1手詰め、3手詰め、5手詰め...）
        for depth in (1..=max_depth).step_by(2) {
            let mut moves = Vec::new();
            if self.search_mate(board, hands, attacker, depth, &mut moves) {
                return (true, moves);
            }

            if self.is_timeout() {
                break;
            }
        }

        (false, Vec::new())
    }

    // 攻め方の手番での探索
    fn search_mate(
        &mut self,
        board: &Board,
        hands: &Hands,
        attacker: Player,
        depth: u8,
        moves: &mut Vec<Move>,
    ) -> bool {
        self.node_count += 1;

        if self.is_timeout() {
            return false;
        }

        if depth == 1 {
            // 1手詰めの判定
            return self.search_one_move_mate(board, hands, attacker, moves);
        }

        // 攻め方の全ての合法手を生成
        let legal_moves = self.generate_all_moves(board, hands, attacker);

        for mv in legal_moves {
            // 手を実行
            let (new_board, new_hands) = self.apply_move(board, hands, &mv, attacker);

            moves.push(mv.clone());

            // 相手が詰んでいるかチェック
            if self.is_checkmate(&new_board, &new_hands, attacker.opponent()) {
                return true;
            }

            // 受け方の応手を探索
            if self.search_defense(
                &new_board,
                &new_hands,
                attacker.opponent(),
                depth - 1,
                moves,
            ) {
                return true;
            }

            // 手を戻す
            moves.pop();
        }

        false
    }

    // 受け方の手番での探索
    fn search_defense(
        &mut self,
        board: &Board,
        hands: &Hands,
        defender: Player,
        depth: u8,
        moves: &mut Vec<Move>,
    ) -> bool {
        self.node_count += 1;

        if self.is_timeout() {
            return false;
        }

        // 受け方の全ての合法手を生成
        let legal_moves = self.generate_all_moves(board, hands, defender);

        // 合法手がない場合は詰み
        if legal_moves.is_empty() {
            return true;
        }

        // 全ての応手に対して詰みがあるかチェック
        for mv in legal_moves {
            let (new_board, new_hands) = self.apply_move(board, hands, &mv, defender);

            moves.push(mv.clone());

            // 攻め方の次の手を探索
            let is_mate = self.search_mate(
                &new_board,
                &new_hands,
                defender.opponent(),
                depth - 1,
                moves,
            );

            moves.pop();

            // 詰まない応手が見つかった
            if !is_mate {
                return false;
            }
        }

        // 全ての応手で詰む
        true
    }

    // 1手詰めを探索
    fn search_one_move_mate(
        &mut self,
        board: &Board,
        hands: &Hands,
        attacker: Player,
        moves: &mut Vec<Move>,
    ) -> bool {
        let legal_moves = self.generate_all_moves(board, hands, attacker);

        for mv in legal_moves {
            let (new_board, new_hands) = self.apply_move(board, hands, &mv, attacker);

            // 相手が詰んでいるかチェック
            if self.is_checkmate(&new_board, &new_hands, attacker.opponent()) {
                moves.push(mv);
                return true;
            }
        }

        false
    }

    // タイムアウトチェック
    fn is_timeout(&self) -> bool {
        (self.performance.now() - self.start_time) > self.timeout_ms as f64
    }

    // 合法手生成（簡略版）
    fn generate_all_moves(&self, _board: &Board, _hands: &Hands, _player: Player) -> Vec<Move> {
        // TODO: 実際の合法手生成を実装
        // 現在は仮実装
        Vec::new()
    }

    // 手を適用（簡略版）
    fn apply_move(
        &self,
        board: &Board,
        hands: &Hands,
        _move: &Move,
        _player: Player,
    ) -> (Board, Hands) {
        // TODO: 実際の手の適用を実装
        // 現在は仮実装
        (
            board.clone(),
            Hands {
                black: hands.black.clone(),
                white: hands.white.clone(),
            },
        )
    }

    // 詰みチェック（簡略版）
    fn is_checkmate(&self, _board: &Board, _hands: &Hands, _player: Player) -> bool {
        // TODO: 実際の詰みチェックを実装
        // 現在は仮実装
        false
    }
}

// WASM インターフェース
#[wasm_bindgen]
pub struct MateSearcher {
    engine: MateSearchEngine,
}

#[wasm_bindgen]
impl MateSearcher {
    #[wasm_bindgen(constructor)]
    pub fn new(timeout_ms: Option<u32>) -> MateSearcher {
        MateSearcher {
            engine: MateSearchEngine::new(timeout_ms.unwrap_or(30000)),
        }
    }

    // 盤面データをJSONで受け取り、探索を実行
    pub fn search_from_json(
        &mut self,
        _board_json: &str,
        _hands_json: &str,
        attacker: &str,
        max_depth: u8,
    ) -> MateSearchResult {
        let start_time = self.engine.performance.now();

        // TODO: JSONパース処理を実装
        let board = Board::new();
        let hands = Hands {
            black: Hand::new(),
            white: Hand::new(),
        };
        let attacker = if attacker == "black" {
            Player::Black
        } else {
            Player::White
        };

        let (is_mate, moves) = self.engine.search(&board, &hands, attacker, max_depth);

        MateSearchResult {
            is_mate,
            move_count: moves.len(),
            node_count: self.engine.node_count,
            elapsed_ms: (self.engine.performance.now() - start_time) as u32,
        }
    }
}
