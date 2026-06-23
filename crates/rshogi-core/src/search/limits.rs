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

    /// 暴走探索を engine 自身が打ち切れる予算（時間・ノード・infinite）を持つか。
    ///
    /// `false`（深さ／詰み目標の完了のみで停止し、時間もノードも `stop` も効かない探索）の場合、
    /// Singular Extension の double/triple 延長が置換表飽和下で連鎖して探索木が
    /// 深さ方向に成長し続けても止める手段が無く、`go depth N` が事実上終了しなくなる。
    /// この判定が `false` のときだけ SE 延長を単延長に制限して終了を保証する。
    ///
    /// 「実際に enforce される停止条件」のみを予算とみなす:
    /// - `use_time_management()`: 純粋な時間管理が有効で時間で停止する（`go depth N btime T` の
    ///   ように depth 併用だと時間管理は無効化されるため `true` にならず、この場合は cap 対象）。
    ///   `rtime` は `time_manager.init` でこの判定が真のときだけ反映されるため（depth 併用時は
    ///   `!use_time_management()` の早期 return より後で処理され enforce されない）、ここでは
    ///   独立項を持たず `use_time_management()` に内包する。
    /// - movetime: 固定思考時間。`time_manager.init` の最初に処理され depth 併用でも enforce される。
    /// - nodes: ノード数で停止。
    /// - infinite: `stop` で打ち切り可能。
    ///
    /// なお `go perft N` は `use_time_management()` が `false` になり「予算なし」扱いだが、
    /// perft は alpha-beta を通らず SE に到達しないため cap は発火しない（実害なし）。
    #[inline]
    pub fn has_interrupt_budget(&self) -> bool {
        self.use_time_management() || self.movetime != 0 || self.nodes != 0 || self.infinite
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
    fn test_has_interrupt_budget() {
        // 純粋な depth 固定（go depth N）・詰み探索は engine 自身の打ち切り予算なし → cap 対象
        let mut l = LimitsType::new();
        l.depth = 15;
        assert!(!l.has_interrupt_budget());
        let mut l = LimitsType::new();
        l.mate = 5;
        assert!(!l.has_interrupt_budget());

        // 純粋な時間管理（depth 併用なし）は予算あり。increment のみ（go binc）・rtime 単独も時間管理が有効。
        for set in [
            |l: &mut LimitsType| l.time[Color::Black.index()] = 60000,
            |l: &mut LimitsType| l.byoyomi[Color::Black.index()] = 30000,
            |l: &mut LimitsType| l.inc[Color::Black.index()] = 1000,
            |l: &mut LimitsType| l.rtime = 1000,
        ] {
            let mut l = LimitsType::new();
            set(&mut l);
            assert!(l.has_interrupt_budget());
        }

        // movetime / nodes / infinite は depth 併用でも enforce されるため予算あり
        for set in [
            |l: &mut LimitsType| l.movetime = 1000,
            |l: &mut LimitsType| l.nodes = 100000,
            |l: &mut LimitsType| l.infinite = true,
        ] {
            let mut l = LimitsType::new();
            l.depth = 15;
            set(&mut l);
            assert!(l.has_interrupt_budget());
        }

        // depth + rtime は time_manager.init で rtime 処理前に !use_time_management() で早期 return
        // され rtime が enforce されない → 予算なし扱い（cap 対象）
        let mut l = LimitsType::new();
        l.depth = 15;
        l.rtime = 1000;
        assert!(!l.has_interrupt_budget());

        // depth と残り時間/秒読みの併用は時間管理が無効化される（time MAX/2 で実 enforce されない）
        // ため予算なし扱い → cap 対象とし、この組合せでもハングしないようにする
        let mut l = LimitsType::new();
        l.depth = 15;
        l.time[Color::Black.index()] = 60000;
        assert!(!l.has_interrupt_budget());
        let mut l = LimitsType::new();
        l.depth = 15;
        l.byoyomi[Color::Black.index()] = 30000;
        assert!(!l.has_interrupt_budget());
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
