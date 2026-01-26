//! 探索制限（LimitsType）
//!
//! USI `go` コマンドのパラメータを表現する。

use crate::time::Instant;
use crate::types::Color;

// =============================================================================
// TimePoint
// =============================================================================

/// 時間（ミリ秒）
pub type TimePoint = i64;

// =============================================================================
// LimitsType
// =============================================================================

/// 探索制限条件
///
/// USI `go` コマンドで指定されるパラメータを保持する。
#[derive(Clone)]
pub struct LimitsType {
    /// 両者の残り時間（ミリ秒）
    pub time: [TimePoint; Color::NUM],

    /// フィッシャールール：1手ごとの時間増加（ミリ秒）
    pub inc: [TimePoint; Color::NUM],

    /// 秒読み時間（ミリ秒）- 将棋独自
    pub byoyomi: [TimePoint; Color::NUM],

    /// 思考時間固定（ミリ秒、0以外なら有効）
    pub movetime: TimePoint,

    /// rtime: ランダム化された固定思考時間（ミリ秒）
    /// YaneuraOu互換の go rtime 用
    pub rtime: TimePoint,

    /// 探索深さ固定（0以外なら有効）
    pub depth: i32,

    /// 詰み専用探索の手数（0以外なら有効）
    /// USI `go mate N` の N は手数（1手=先後1回ずつ）で、内部比較では 2*N 手目まで探索する。
    pub mate: i32,

    /// perftテスト中のフラグ（非0なら深さ）
    pub perft: i32,

    /// 思考時間無制限フラグ
    pub infinite: bool,

    /// 探索ノード数制限（0以外なら有効）
    pub nodes: u64,

    /// ponder有効フラグ
    pub ponder: bool,

    /// MultiPV（候補手複数探索）の数（1以上、デフォルト1）
    /// YaneuraOu準拠: 複数の候補手を探索して表示する
    pub multi_pv: usize,

    /// 探索対象の手のリスト
    /// 空なら全合法手を探索
    pub search_moves: Vec<crate::types::Move>,

    /// 探索開始時刻
    pub(crate) start_time: Option<Instant>,
}

impl Default for LimitsType {
    fn default() -> Self {
        Self {
            time: [0; Color::NUM],
            inc: [0; Color::NUM],
            byoyomi: [0; Color::NUM],
            movetime: 0,
            rtime: 0,
            depth: 0,
            mate: 0,
            perft: 0,
            infinite: false,
            nodes: 0,
            ponder: false,
            multi_pv: 1, // デフォルトは1（通常探索）
            search_moves: Vec::new(),
            start_time: None,
        }
    }
}

impl LimitsType {
    /// 新しいLimitsTypeを作成
    pub fn new() -> Self {
        Self::default()
    }

    /// 時間制御を行うべきかの判定
    ///
    /// 以下のいずれかが指定されている場合は時間制御を行わない：
    /// - mate（詰み探索）
    /// - movetime（固定思考時間）
    /// - depth（固定深さ）
    /// - nodes（ノード数制限）
    /// - perft（perftテスト）
    /// - infinite（無制限）
    #[inline]
    pub fn use_time_management(&self) -> bool {
        self.mate == 0
            && self.movetime == 0
            && self.depth == 0
            && self.nodes == 0
            && self.perft == 0
            && !self.infinite
    }

    /// 探索開始時刻を設定
    pub fn set_start_time(&mut self) {
        self.start_time = Some(Instant::now());
    }

    /// 探索開始からの経過時間（ミリ秒）
    pub fn elapsed(&self) -> TimePoint {
        self.start_time.map(|t| t.elapsed().as_millis() as TimePoint).unwrap_or(0)
    }

    /// 指定した色の残り時間を取得
    #[inline]
    pub fn time_left(&self, color: Color) -> TimePoint {
        self.time[color.index()]
    }

    /// 指定した色の秒読み時間を取得
    #[inline]
    pub fn byoyomi_time(&self, color: Color) -> TimePoint {
        self.byoyomi[color.index()]
    }

    /// 指定した色のインクリメント時間を取得
    #[inline]
    pub fn increment(&self, color: Color) -> TimePoint {
        self.inc[color.index()]
    }

    /// 深さ制限があるか
    #[inline]
    pub fn has_depth_limit(&self) -> bool {
        self.depth > 0
    }

    /// ノード数制限があるか
    #[inline]
    pub fn has_nodes_limit(&self) -> bool {
        self.nodes > 0
    }

    /// 思考時間が固定されているか
    #[inline]
    pub fn has_movetime(&self) -> bool {
        self.movetime > 0
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_default() {
        let limits = LimitsType::default();
        assert_eq!(limits.time[0], 0);
        assert_eq!(limits.time[1], 0);
        assert_eq!(limits.depth, 0);
        assert_eq!(limits.rtime, 0);
        assert!(!limits.infinite);
        assert!(limits.search_moves.is_empty());
    }

    #[test]
    fn test_use_time_management() {
        let mut limits = LimitsType::new();

        // デフォルトでは時間制御を行う（残り時間がある場合を想定）
        assert!(limits.use_time_management());

        // 深さ固定なら時間制御しない
        limits.depth = 10;
        assert!(!limits.use_time_management());
        limits.depth = 0;

        // 無制限なら時間制御しない
        limits.infinite = true;
        assert!(!limits.use_time_management());
        limits.infinite = false;

        // ノード数制限なら時間制御しない
        limits.nodes = 10000;
        assert!(!limits.use_time_management());
        limits.nodes = 0;

        // 思考時間固定なら時間制御しない
        limits.movetime = 1000;
        assert!(!limits.use_time_management());
    }

    #[test]
    fn test_time_left() {
        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = 60000; // 1分
        limits.time[Color::White.index()] = 30000; // 30秒

        assert_eq!(limits.time_left(Color::Black), 60000);
        assert_eq!(limits.time_left(Color::White), 30000);
    }

    #[test]
    fn test_byoyomi() {
        let mut limits = LimitsType::new();
        limits.byoyomi[Color::Black.index()] = 30000; // 30秒

        assert_eq!(limits.byoyomi_time(Color::Black), 30000);
        assert_eq!(limits.byoyomi_time(Color::White), 0);
    }

    #[test]
    fn test_elapsed() {
        let mut limits = LimitsType::new();
        limits.set_start_time();

        // 少し待つ
        std::thread::sleep(std::time::Duration::from_millis(10));

        let elapsed = limits.elapsed();
        assert!(elapsed >= 10);
        assert!(elapsed < 1000); // 1秒以内
    }

    #[test]
    fn test_has_limits() {
        let mut limits = LimitsType::new();

        assert!(!limits.has_depth_limit());
        assert!(!limits.has_nodes_limit());
        assert!(!limits.has_movetime());

        limits.depth = 10;
        assert!(limits.has_depth_limit());

        limits.nodes = 10000;
        assert!(limits.has_nodes_limit());

        limits.movetime = 1000;
        assert!(limits.has_movetime());
    }
}
