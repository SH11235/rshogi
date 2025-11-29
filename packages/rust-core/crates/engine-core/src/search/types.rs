//! 探索で使用する基本型
//!
//! - `NodeType`: ノードの種類（Root, PV, NonPV）
//! - `Stack`: 探索スタック
//! - `RootMove`: ルート手の情報
//! - `RootMoves`: ルート手のリスト

use crate::movegen::{generate_legal, MoveList};
use crate::position::Position;
use crate::types::{Move, Value, MAX_PLY};

// =============================================================================
// 定数
// =============================================================================

/// 探索スタックのサイズ（MAX_PLY + マージン）
pub const STACK_SIZE: usize = MAX_PLY as usize + 10;

// =============================================================================
// NodeType
// =============================================================================

/// ノードの種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// ルートノード
    Root,
    /// Principal Variationノード（最善手が期待されるノード）
    PV,
    /// 非PVノード
    NonPV,
}

impl NodeType {
    /// PVノードか（Root | PV）
    #[inline]
    pub const fn is_pv(self) -> bool {
        matches!(self, Self::Root | Self::PV)
    }
}

// =============================================================================
// Stack（探索スタック）
// =============================================================================

/// 探索時の各ノードの状態
#[derive(Clone)]
pub struct Stack {
    /// PV（Principal Variation）
    pub pv: Vec<Move>,

    /// ContinuationHistoryへの参照インデックス
    pub cont_history_idx: usize,

    /// ルートからの手数
    pub ply: i32,

    /// このノードで選択されている手
    pub current_move: Move,

    /// Singular Extension用の除外手
    pub excluded_move: Move,

    /// 評価関数の値
    pub static_eval: Value,

    /// History統計のスコア（キャッシュ）
    pub stat_score: i32,

    /// このノードで調べた手の数
    pub move_count: i32,

    /// 王手がかかっているか
    pub in_check: bool,

    /// TTエントリがPVノードからのものか
    pub tt_pv: bool,

    /// TTにヒットしたか
    pub tt_hit: bool,

    /// βカットした回数
    pub cutoff_cnt: i32,

    /// このノードでのreduction量
    pub reduction: i32,

    /// quietな手が連続した回数
    pub quiet_move_streak: i32,
}

impl Default for Stack {
    fn default() -> Self {
        Self {
            pv: Vec::new(),
            cont_history_idx: 0,
            ply: 0,
            current_move: Move::NONE,
            excluded_move: Move::NONE,
            static_eval: Value::NONE,
            stat_score: 0,
            move_count: 0,
            in_check: false,
            tt_pv: false,
            tt_hit: false,
            cutoff_cnt: 0,
            reduction: 0,
            quiet_move_streak: 0,
        }
    }
}

impl Stack {
    /// 新しいスタックを作成
    pub fn new() -> Self {
        Self::default()
    }

    /// plyを設定して新しいスタックを作成
    pub fn with_ply(ply: i32) -> Self {
        Self {
            ply,
            ..Self::default()
        }
    }

    /// PVをクリア
    pub fn clear_pv(&mut self) {
        self.pv.clear();
    }

    /// PVを更新（best_moveを先頭に、child_pvを続ける）
    pub fn update_pv(&mut self, best_move: Move, child_pv: &[Move]) {
        self.pv.clear();
        self.pv.push(best_move);
        self.pv.extend_from_slice(child_pv);
    }
}

/// 探索で使用するスタック配列
pub type StackArray = [Stack; STACK_SIZE];

/// StackArrayを初期化
pub fn init_stack_array() -> StackArray {
    std::array::from_fn(|i| Stack::with_ply(i as i32))
}

// =============================================================================
// RootMove（ルート手の情報）
// =============================================================================

/// ルートでの指し手情報
#[derive(Clone)]
pub struct RootMove {
    /// 探索スコア
    pub score: Value,
    /// 前回のスコア
    pub previous_score: Value,
    /// 平均スコア
    pub average_score: Value,
    /// スコアの下界フラグ
    pub score_lower_bound: bool,
    /// スコアの上界フラグ
    pub score_upper_bound: bool,
    /// 選択深さ（最大到達深度）
    pub sel_depth: i32,
    /// この手の探索にかかったeffort（ノード数の割合）
    pub effort: f64,
    /// PV（Principal Variation）
    /// pv[0]が指し手自体
    pub pv: Vec<Move>,
}

impl RootMove {
    /// 指し手から新しいRootMoveを作成
    pub fn new(mv: Move) -> Self {
        Self {
            score: Value::new(-32001), // MINUS_INFINITE相当
            previous_score: Value::new(-32001),
            average_score: Value::new(-32001),
            score_lower_bound: false,
            score_upper_bound: false,
            sel_depth: 0,
            effort: 0.0,
            pv: vec![mv],
        }
    }

    /// 指し手を取得
    #[inline]
    pub fn mv(&self) -> Move {
        self.pv[0]
    }

    /// PVを更新
    pub fn update_pv(&mut self, child_pv: &[Move]) {
        // pv[0]（自分自身の手）は保持し、child_pvを追加
        self.pv.truncate(1);
        self.pv.extend_from_slice(child_pv);
    }

    /// 置換表からポンダー手を抽出（TODO: TT連携時に実装）
    pub fn extract_ponder_from_tt(&mut self, _pos: &Position) -> bool {
        // Phase 6c以降で実装
        false
    }
}

impl PartialEq for RootMove {
    fn eq(&self, other: &Self) -> bool {
        self.pv[0] == other.pv[0]
    }
}

impl Eq for RootMove {}

/// スコアの降順でソート（同スコアなら previous_score で比較）
impl PartialOrd for RootMove {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RootMove {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 降順ソート: スコアが高い方が先
        match other.score.raw().cmp(&self.score.raw()) {
            std::cmp::Ordering::Equal => other.previous_score.raw().cmp(&self.previous_score.raw()),
            ord => ord,
        }
    }
}

// =============================================================================
// RootMoves（ルート手のリスト）
// =============================================================================

/// ルート局面での候補手リスト
pub struct RootMoves {
    moves: Vec<RootMove>,
}

impl RootMoves {
    /// 空のRootMovesを作成
    pub fn new() -> Self {
        Self { moves: Vec::new() }
    }

    /// 合法手からRootMovesを初期化
    ///
    /// # Arguments
    /// * `pos` - 現在の局面
    /// * `search_moves` - 探索対象の手（空なら全合法手）
    pub fn from_legal_moves(pos: &Position, search_moves: &[Move]) -> Self {
        let mut legal_moves = MoveList::new();
        generate_legal(pos, &mut legal_moves);
        let mut moves = Vec::new();

        for &mv in legal_moves.as_slice() {
            // search_movesが指定されていれば、その中にある手のみ
            if search_moves.is_empty() || search_moves.contains(&mv) {
                moves.push(RootMove::new(mv));
            }
        }

        Self { moves }
    }

    /// 手の数
    #[inline]
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// 空かどうか
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// イテレータ
    pub fn iter(&self) -> impl Iterator<Item = &RootMove> {
        self.moves.iter()
    }

    /// 可変イテレータ
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut RootMove> {
        self.moves.iter_mut()
    }

    /// インデックスアクセス
    #[inline]
    pub fn get(&self, index: usize) -> Option<&RootMove> {
        self.moves.get(index)
    }

    /// 可変インデックスアクセス
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut RootMove> {
        self.moves.get_mut(index)
    }

    /// 最善手を先頭に移動
    pub fn move_to_front(&mut self, idx: usize) {
        if idx > 0 && idx < self.moves.len() {
            let rm = self.moves.remove(idx);
            self.moves.insert(0, rm);
        }
    }

    /// スコアでソート（降順）
    pub fn sort(&mut self) {
        self.moves.sort();
    }

    /// 指定した手を含むか
    pub fn contains(&self, mv: Move) -> bool {
        self.moves.iter().any(|rm| rm.mv() == mv)
    }

    /// 指定した手のインデックスを取得
    pub fn find(&self, mv: Move) -> Option<usize> {
        self.moves.iter().position(|rm| rm.mv() == mv)
    }

    /// 内部Vecへの参照
    pub fn as_slice(&self) -> &[RootMove] {
        &self.moves
    }

    /// 内部Vecへの可変参照
    pub fn as_mut_slice(&mut self) -> &mut [RootMove] {
        &mut self.moves
    }
}

impl Default for RootMoves {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Index<usize> for RootMoves {
    type Output = RootMove;

    fn index(&self, index: usize) -> &Self::Output {
        &self.moves[index]
    }
}

impl std::ops::IndexMut<usize> for RootMoves {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.moves[index]
    }
}

// =============================================================================
// TT値の変換（詰みスコア補正）
// =============================================================================

/// 探索値をTT保存用に変換（詰みスコアをply基準からroot基準に変換）
///
/// 詰みスコアは「あと何手で詰むか」を表すが、TTに保存する際は
/// ルートからの手数を補正する必要がある。
#[inline]
pub fn value_to_tt(v: Value, ply: i32) -> Value {
    if v.is_win() {
        Value::new(v.raw() + ply)
    } else if v.is_loss() {
        Value::new(v.raw() - ply)
    } else {
        v
    }
}

/// TT値を探索用に変換（詰みスコアをroot基準からply基準に変換）
#[inline]
pub fn value_from_tt(v: Value, ply: i32) -> Value {
    if v.is_win() {
        Value::new(v.raw() - ply)
    } else if v.is_loss() {
        Value::new(v.raw() + ply)
    } else {
        v
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_type_is_pv() {
        assert!(NodeType::Root.is_pv());
        assert!(NodeType::PV.is_pv());
        assert!(!NodeType::NonPV.is_pv());
    }

    #[test]
    fn test_stack_default() {
        let stack = Stack::default();
        assert!(stack.pv.is_empty());
        assert_eq!(stack.ply, 0);
        assert!(stack.current_move.is_none());
        assert!(!stack.in_check);
    }

    #[test]
    fn test_stack_update_pv() {
        let mut stack = Stack::default();
        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("3c3d").unwrap();
        let mv3 = Move::from_usi("2g2f").unwrap();

        stack.update_pv(mv1, &[mv2, mv3]);

        assert_eq!(stack.pv.len(), 3);
        assert_eq!(stack.pv[0], mv1);
        assert_eq!(stack.pv[1], mv2);
        assert_eq!(stack.pv[2], mv3);
    }

    #[test]
    fn test_root_move_new() {
        let mv = Move::from_usi("7g7f").unwrap();
        let rm = RootMove::new(mv);

        assert_eq!(rm.mv(), mv);
        assert_eq!(rm.pv.len(), 1);
        assert!(rm.score.raw() < 0);
    }

    #[test]
    fn test_root_move_ordering() {
        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();

        let mut rm1 = RootMove::new(mv1);
        let mut rm2 = RootMove::new(mv2);

        rm1.score = Value::new(100);
        rm2.score = Value::new(50);

        // 降順ソート: 高スコア（rm1）が先 = 高スコアが「小さい」
        // rm1(100) vs rm2(50): rm1 が先に来るので rm1 < rm2
        assert!(rm1 < rm2, "高スコアが先（小さい）になるべき");

        // スコアを同じにして previous_score でテスト
        rm1.score = Value::new(100);
        rm2.score = Value::new(100);
        rm1.previous_score = Value::new(80);
        rm2.previous_score = Value::new(90);

        // 降順ソート: 高previous_score（rm2）が先 = rm2 < rm1
        assert!(rm2 < rm1, "同スコア時は高previous_scoreが先（小さい）");
    }

    #[test]
    fn test_root_moves_basic() {
        let mut rm = RootMoves::new();
        assert!(rm.is_empty());

        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();

        rm.moves.push(RootMove::new(mv1));
        rm.moves.push(RootMove::new(mv2));

        assert_eq!(rm.len(), 2);
        assert!(rm.contains(mv1));
        assert!(rm.contains(mv2));
        assert_eq!(rm.find(mv1), Some(0));
        assert_eq!(rm.find(mv2), Some(1));
    }

    #[test]
    fn test_root_moves_move_to_front() {
        let mut rm = RootMoves::new();
        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();
        let mv3 = Move::from_usi("3g3f").unwrap();

        rm.moves.push(RootMove::new(mv1));
        rm.moves.push(RootMove::new(mv2));
        rm.moves.push(RootMove::new(mv3));

        rm.move_to_front(2);

        assert_eq!(rm[0].mv(), mv3);
        assert_eq!(rm[1].mv(), mv1);
        assert_eq!(rm[2].mv(), mv2);
    }

    #[test]
    fn test_value_to_tt_from_tt_roundtrip() {
        // 通常値
        let v = Value::new(100);
        let ply = 5;
        assert_eq!(value_from_tt(value_to_tt(v, ply), ply), v);

        // 勝ちスコア
        let win = Value::mate_in(10);
        let converted = value_to_tt(win, ply);
        let restored = value_from_tt(converted, ply);
        assert_eq!(restored.raw(), win.raw());

        // 負けスコア
        let loss = Value::mated_in(10);
        let converted = value_to_tt(loss, ply);
        let restored = value_from_tt(converted, ply);
        assert_eq!(restored.raw(), loss.raw());
    }

    #[test]
    fn test_init_stack_array() {
        let stack = init_stack_array();
        assert_eq!(stack.len(), STACK_SIZE);

        for (i, s) in stack.iter().enumerate() {
            assert_eq!(s.ply, i as i32);
        }
    }
}
