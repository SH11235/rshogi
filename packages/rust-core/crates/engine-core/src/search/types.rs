//! 探索で使用する基本型
//!
//! - `NodeType`: ノードの種類（Root, PV, NonPV）
//! - `Stack`: 探索スタック
//! - `RootMove`: ルート手の情報
//! - `RootMoves`: ルート手のリスト

use crate::movegen::{generate_legal, MoveList};
use crate::position::Position;
use crate::types::{Move, Piece, RepetitionState, Square, Value, MAX_PLY};

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
// ContHistKey（ContinuationHistoryキー）
// =============================================================================

/// ContinuationHistoryを参照するためのキー情報
///
/// YaneuraOu方式: 各ノードで指し手実行後に設定し、
/// 後続のノードでContinuationHistoryテーブルを参照する際に使用する。
#[derive(Clone, Copy, Debug)]
pub struct ContHistKey {
    /// 王手がかかっているか
    pub in_check: bool,
    /// 駒取りの手か
    pub capture: bool,
    /// 移動した駒（成り後の駒）
    pub piece: Piece,
    /// 移動先のマス
    pub to: Square,
}

impl ContHistKey {
    /// 新しいContHistKeyを作成
    pub fn new(in_check: bool, capture: bool, piece: Piece, to: Square) -> Self {
        Self {
            in_check,
            capture,
            piece,
            to,
        }
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

    /// ContinuationHistoryへの参照インデックス（旧方式、互換性のため残す）
    pub cont_history_idx: usize,

    /// ContinuationHistoryキー（YaneuraOu方式）
    /// do_move後に設定し、後続ノードでContinuationHistory参照に使用
    pub cont_hist_key: Option<ContHistKey>,

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
            cont_hist_key: None,
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
// SmallMoveList（固定長の指し手リスト）
// =============================================================================

/// 固定長の指し手リスト
///
/// YaneuraOu準拠のSEARCHEDLIST_CAPACITY（32手）をベースに設計。
/// ヒープ割り当てを避け、探索ホットパスでの性能を向上させる。
///
/// 目的は「そのノードで試した全手の保存」ではなく、
/// 「統計更新のための代表集合」の保存。
/// 固定長の指し手リスト
///
/// YaneuraOu準拠のSEARCHEDLIST_CAPACITY（32手）をベースに設計。
/// ヒープ割り当てを避け、探索ホットパスでの性能を向上させる。
///
/// 目的は「そのノードで試した全手の保存」ではなく、
/// 「統計更新のための代表集合」の保存。
/// 例: history テーブルやkillerムーブの更新に使用する手のリスト。
/// 32手を超える場合は古い統計情報で十分と判断される。
    buf: [Move; N],
    len: usize,
}

impl<const N: usize> SmallMoveList<N> {
    /// 空のSmallMoveListを作成
    #[inline]
    pub fn new() -> Self {
        Self {
            buf: [Move::NONE; N],
            len: 0,
        }
    }

    /// 指し手を追加
    ///
    /// 容量を超えた場合は無視する（YaneuraOu準拠: 32手を超える分は記録しない）
    #[inline]
    pub fn push(&mut self, mv: Move) {
        if self.len < N {
            self.buf[self.len] = mv;
            self.len += 1;
        }
    }

    /// 現在の要素数
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 空かどうか
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// イテレータを返す
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Move> {
        self.buf[..self.len].iter()
    }
}

impl<const N: usize> Default for SmallMoveList<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// 探索用の固定長指し手リスト（YaneuraOu SEARCHEDLIST_CAPACITY相当）
pub const SEARCHED_MOVES_CAPACITY: usize = 32;

/// quiets_tried / captures_tried 用の型エイリアス
pub type SearchedMoveList = SmallMoveList<SEARCHED_MOVES_CAPACITY>;

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
    /// 二乗平均スコア（aspiration window用）
    pub mean_squared_score: Option<i64>,
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
            mean_squared_score: None,
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

    /// 平均スコア・二乗平均スコアを蓄積（YaneuraOuのaspiration初期窓用）
    pub fn accumulate_score_stats(&mut self, value: Value) {
        // average_score: 初回はそのまま、2回目以降は前回と現在の平均
        self.average_score = if self.average_score.raw() == -Value::INFINITE.raw() {
            value
        } else {
            let avg = (self.average_score.raw() + value.raw()) / 2;
            Value::new(avg)
        };

        // mean_squared_score: |value| * value を平均
        let sample = (value.raw() as i64) * (value.raw().abs() as i64);
        self.mean_squared_score = Some(match self.mean_squared_score {
            Some(prev) => (prev + sample) / 2,
            None => sample,
        });
    }
}

impl PartialEq for RootMove {
    fn eq(&self, other: &Self) -> bool {
        self.pv[0] == other.pv[0]
    }
}

impl Eq for RootMove {}

/// スコアの降順でソート（YaneuraOu準拠: score優先、同点はprevious_score）
impl PartialOrd for RootMove {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RootMove {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 降順ソート: スコアが高い方が先
        // YaneuraOu準拠: スコア同点時はprevious_scoreで比較
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

    /// テスト用: 指定されたRootMoveで初期化
    #[cfg(test)]
    pub(crate) fn from_vec(moves: Vec<RootMove>) -> Self {
        Self { moves }
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

    /// 指定インデックスの要素を別のインデックスに移動
    ///
    /// from_idxの要素をremoveして、to_idxにinsertする。
    /// MultiPVループで使用。
    ///
    /// # Arguments
    /// * `from_idx` - 移動元インデックス
    /// * `to_idx` - 移動先インデックス
    pub fn move_to_index(&mut self, from_idx: usize, to_idx: usize) {
        if from_idx != to_idx && from_idx < self.moves.len() {
            let rm = self.moves.remove(from_idx);
            let insert_idx = to_idx.min(self.moves.len());
            self.moves.insert(insert_idx, rm);
        }
    }

    /// スコアでソート（降順）
    pub fn sort(&mut self) {
        self.moves.sort();
    }

    /// 指定範囲をスコア降順で安定ソート
    ///
    /// YaneuraOuの std::stable_sort に相当。
    /// 同じスコアの場合、元の順序を保持する。
    ///
    /// # Arguments
    /// * `start` - ソート開始インデックス
    /// * `end` - ソート終了インデックス（この要素は含まない）
    pub fn stable_sort_range(&mut self, start: usize, end: usize) {
        if start >= end || end > self.moves.len() {
            return;
        }

        // インデックス付きソート: (元のindex, スコア)でソート
        let mut indexed: Vec<(usize, Value, Value)> = self.moves[start..end]
            .iter()
            .enumerate()
            .map(|(i, rm)| (start + i, rm.score, rm.previous_score))
            .collect();

        // スコア降順、同点ならprevious_score降順、それでも同点なら元のインデックス昇順（安定性）
        indexed.sort_by(|a, b| match b.1.cmp(&a.1) {
            std::cmp::Ordering::Equal => match b.2.cmp(&a.2) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ord => ord,
            },
            ord => ord,
        });

        // ソート結果を適用
        let sorted_moves: Vec<RootMove> =
            indexed.iter().map(|(idx, _, _)| self.moves[*idx].clone()).collect();

        self.moves[start..end].clone_from_slice(&sorted_moves);
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

/// 千日手/優劣局面を評価値に変換（YaneuraOu簡易版）
#[inline]
pub fn draw_value(state: RepetitionState, _stm: crate::types::Color) -> Value {
    match state {
        RepetitionState::Draw => Value::DRAW,
        RepetitionState::Win => Value::MATE,
        RepetitionState::Lose => -Value::MATE,
        RepetitionState::Superior => Value::MATE_IN_MAX_PLY,
        RepetitionState::Inferior => Value::MATED_IN_MAX_PLY,
        RepetitionState::None => Value::NONE,
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

        // YaneuraOu準拠: スコア同点時はprevious_scoreでも比較
        rm1.score = Value::new(100);
        rm2.score = Value::new(100);
        rm1.previous_score = Value::new(80);
        rm2.previous_score = Value::new(90);

        // スコア同点時はprevious_scoreで比較（rm2の方が大きいので降順ソートでrm2が先）
        assert_eq!(
            rm1.cmp(&rm2),
            std::cmp::Ordering::Greater,
            "previous_score: 80 < 90 なので、降順ソートでrm2が先"
        );

        // スコアもprevious_scoreも同じ場合はEqual
        rm1.previous_score = Value::new(90);
        assert_eq!(
            rm1.cmp(&rm2),
            std::cmp::Ordering::Equal,
            "スコアもprevious_scoreも同じ場合はEqual（YaneuraOu準拠）"
        );
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

    #[test]
    fn test_root_move_accumulate_score_stats() {
        let mv = Move::from_usi("7g7f").unwrap();
        let mut rm = RootMove::new(mv);

        // 初回はそのまま反映
        rm.accumulate_score_stats(Value::new(100));
        assert_eq!(rm.average_score.raw(), 100);
        assert_eq!(rm.mean_squared_score, Some(10_000));

        // 2回目以降は平均を取る
        rm.accumulate_score_stats(Value::new(-60));
        assert_eq!(rm.average_score.raw(), 20); // (100 + -60) / 2
                                                // mean_squared_score は value * |value| を平均するため符号を保持する
        assert_eq!(rm.mean_squared_score, Some((10_000 - 3_600) / 2));
    }

    #[test]
    fn test_small_move_list_basic() {
        let mut list: SmallMoveList<4> = SmallMoveList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();

        list.push(mv1);
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());

        list.push(mv2);
        assert_eq!(list.len(), 2);

        let moves: Vec<_> = list.iter().copied().collect();
        assert_eq!(moves, vec![mv1, mv2]);
    }

    #[test]
    fn test_small_move_list_capacity_limit() {
        let mut list: SmallMoveList<2> = SmallMoveList::new();

        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();
        let mv3 = Move::from_usi("3g3f").unwrap();

        list.push(mv1);
        list.push(mv2);
        assert_eq!(list.len(), 2);

        // 容量を超えても追加は無視される
        list.push(mv3);
        assert_eq!(list.len(), 2);

        let moves: Vec<_> = list.iter().copied().collect();
        assert_eq!(moves, vec![mv1, mv2]);
    }

    #[test]
    fn test_searched_move_list() {
        let mut list = SearchedMoveList::new();
        assert_eq!(list.len(), 0);

        // SEARCHED_MOVES_CAPACITYまで追加可能
        for i in 0..SEARCHED_MOVES_CAPACITY {
            let mv = Move::from_usi("7g7f").unwrap();
            list.push(mv);
            assert_eq!(list.len(), i + 1);
        }

        // 容量を超えると無視される
        let mv = Move::from_usi("2g2f").unwrap();
        list.push(mv);
        assert_eq!(list.len(), SEARCHED_MOVES_CAPACITY);
    }
}
