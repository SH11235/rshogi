//! ratatui ベースの棋譜プレイヤー画面・イベントループ。

use std::collections::BTreeMap;
use std::io::{self};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color as RColor, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move, PieceType, Square};

use crate::kif::piece_label;

use rshogi_csa_client::jsonl::sanitize_for_filename;

use super::model::{
    GameIndexEntry, GameOutcomeView, GameRecord, GameSource, GameSourceRef, MoveView, PairFileMeta,
};
use super::{GameIndex, display_label};

/// TUI 起動オプション。
#[derive(Default)]
pub struct RunOptions {
    /// live 再読込の間隔。指定するとこの間隔でソースのフィンガープリントを確認し、
    /// 変化があれば索引を取り直して新しい対局を一覧へ追加する。`None` は従来どおり
    /// 起動時読み込みのみ。
    pub live_interval: Option<Duration>,
    /// 正規化名(`sanitize_for_filename` 済み) -> レート。空なら R 併記・`rate:`
    /// フィルタは不活性。
    pub ratings: BTreeMap<String, f64>,
}

/// 棋譜プレイヤー TUI を起動する。`Ctrl-C`／`q` で終了するまでブロックする。
pub fn run(source: Box<dyn GameSource>, opts: RunOptions) -> Result<()> {
    let index = source.build_index()?;
    for warning in &index.warnings {
        eprintln!("warning: {warning}");
    }
    let live = match opts.live_interval {
        Some(interval) => {
            let Some(fingerprint) = source.live_fingerprint()? else {
                anyhow::bail!("--live はこの入力形式では使えません(ディレクトリ横断ソースのみ)");
            };
            Some(LiveState {
                interval,
                last_poll: Instant::now(),
                fingerprint,
            })
        }
        None => None,
    };
    // live 中は「まだ 1 局も無い記録 dir を開いて対局を待つ」使い方を許す。
    if index.entries.is_empty() && live.is_none() {
        anyhow::bail!("対局が1件も見つかりませんでした");
    }

    // raw mode/alternate screen 中に panic すると端末が壊れたまま残るため、
    // 復元してから元の panic hook に委譲する。
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(source, index, opts.ratings, live);
    let result = run_event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

enum Mode {
    Browse,
    Filter,
    Help,
}

/// 対局一覧の並び順。`apply_filter` 実行時に安定ソートで適用する
/// （同じキー内の相対順は発見順を維持する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    /// ファイル列挙順→完了順。
    Discovery,
    /// ファイル名由来の対局日時の降順（新しい対局が先頭）。日時の無い対局は末尾。既定。
    Date,
    /// エラー→黒勝ち→白勝ち→引き分け→不明の順にグルーピング。
    Outcome,
    /// 対局長（手数）の降順。
    Length,
    /// 決着の大きさ（`|final_cp|`）の降順。評価値の無い対局は末尾。
    Decisiveness,
    /// 評価値の振れ幅（`max_swing_cp`）の降順。評価値の無い対局は末尾。
    Swing,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::Discovery => SortMode::Date,
            SortMode::Date => SortMode::Outcome,
            SortMode::Outcome => SortMode::Length,
            SortMode::Length => SortMode::Decisiveness,
            SortMode::Decisiveness => SortMode::Swing,
            SortMode::Swing => SortMode::Discovery,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::Discovery => "発見順",
            SortMode::Date => "日付(新)",
            SortMode::Outcome => "勝敗別",
            SortMode::Length => "対局長",
            SortMode::Decisiveness => "決着の大きさ",
            SortMode::Swing => "評価値振れ幅",
        }
    }
}

/// SFEN 局面検索の 1 tick で走査する対局数。描画と走査のバランスを取る目安値。
const SFEN_SCAN_CHUNK: usize = 32;

/// SFEN 局面検索（逐次スキャン）の進行状態。イベントループが 1 tick ごとに
/// `SFEN_SCAN_CHUNK` 対局ずつ `load_game` して照合し、途中経過を描画する。局面は 1 対局
/// ぶんずつ読んでは捨てるので、追加で保持するのは一致した対局の index（`matches`）だけ
/// ＝メモリは一致件数に比例し、対局あたりの局面数には非依存。
struct SfenScan {
    /// 正規化済み（手数フィールドを除いた盤面・手番・持駒）の検索対象 SFEN。
    target: String,
    /// 次に走査する `index.entries` のインデックス。
    next: usize,
    /// これまでに一致した `index.entries` のインデックス列。
    matches: Vec<usize>,
}

/// live 再読込の進行状態。
struct LiveState {
    interval: Duration,
    /// 最後にフィンガープリントを確認した時刻(間隔制御用)。
    last_poll: Instant,
    /// 前回確認したソースのフィンガープリント。変化検出にのみ使う。
    fingerprint: u64,
}

struct App {
    source: Box<dyn GameSource>,
    index: GameIndex,
    /// 現在の絞り込み結果（`index.entries` への index 列、ソート前・発見順）。メタ情報
    /// フィルタは `entry_matches` から、SFEN 局面検索は逐次スキャンの一致集合から作る。
    /// 並べ替えはこれを再ソートするので、SFEN スキャン結果も並べ替えで失われない。
    base_filtered: Vec<usize>,
    /// `base_filtered` を現在の並び順で表示用に並べたもの。
    filtered: Vec<usize>,
    selected: usize,
    mode: Mode,
    filter_input: String,
    sort_mode: SortMode,
    current_game: Option<GameRecord>,
    current_move: usize,
    status: String,
    /// 実行中の SFEN 局面検索（逐次スキャン）。`None` なら通常操作。
    scan: Option<SfenScan>,
    /// 正規化名 -> レート（`RunOptions::ratings`）。
    ratings: BTreeMap<String, f64>,
    /// live 再読込の状態。`None` なら従来どおり静的表示。
    live: Option<LiveState>,
    /// ライブ追従モード（`f`）。ON の間は選択対局の最新手を表示し続ける。
    /// 末尾から離れて特定の手を見る操作（`h`/`←`、`n`/`N`、`Home`/`g`）で自動解除する
    /// （実際に表示位置が動いたときのみ。末尾での `n` 空振り等では解除しない）。
    follow: bool,
}

impl App {
    fn new(
        source: Box<dyn GameSource>,
        index: GameIndex,
        ratings: BTreeMap<String, f64>,
        live: Option<LiveState>,
    ) -> Self {
        let all: Vec<usize> = (0..index.entries.len()).collect();
        let mut app = Self {
            source,
            index,
            base_filtered: all.clone(),
            filtered: all,
            selected: 0,
            mode: Mode::Browse,
            filter_input: String::new(),
            sort_mode: SortMode::Date,
            current_game: None,
            current_move: 0,
            status: String::new(),
            scan: None,
            ratings,
            live,
            follow: false,
        };
        // 初期表示から既定ソート（日付(新)）を適用する。`resort` が `load_selected` も行う。
        app.resort();
        app
    }

    /// 正規化キーでレートを引く。`--ratings` 未供給なら常に `None`。
    fn rate_of(&self, name: &str) -> Option<f64> {
        if self.ratings.is_empty() {
            return None;
        }
        self.ratings.get(&sanitize_for_filename(name)).copied()
    }

    /// 対局の代表レート（両対局者のうち高い方）。`rate:` フィルタ用。
    fn entry_rate(&self, entry: &GameIndexEntry) -> Option<f64> {
        let meta = file_meta(&self.index, entry)?;
        combined_rate(self.rate_of(&meta.black_label), self.rate_of(&meta.white_label))
    }

    fn selected_entry(&self) -> Option<&GameIndexEntry> {
        self.filtered.get(self.selected).map(|&i| &self.index.entries[i])
    }

    fn load_selected(&mut self) {
        self.current_move = 0;
        self.current_game = None;
        let Some(entry) = self.selected_entry() else {
            return;
        };
        match self.source.load_game(&self.index, entry) {
            Ok(game) => {
                self.status.clear();
                // 追従中は開いた対局の最新手から表示する（j/k での対局切替も含む）。
                if self.follow && !game.moves.is_empty() {
                    self.current_move = game.moves.len() - 1;
                }
                self.current_game = Some(game);
            }
            Err(e) => self.status = format!("対局の読み込みに失敗しました: {e}"),
        }
    }

    fn apply_filter(&mut self) {
        let query = self.filter_input.to_lowercase();
        let filter = parse_filter(&query);
        self.base_filtered = (0..self.index.entries.len())
            .filter(|&i| entry_matches(&self.index, &self.index.entries[i], filter))
            .collect();
        // `rate:` は App の保持するレート表が要るため `entry_matches` の外で絞る
        // (`sfen:` が逐次スキャンで扱われるのと同様、entry_matches では常に一致扱い)。
        // floodgate のレートは負値がありうるので符号付きで比較する。
        if let Filter::Field(FieldKind::Rate, spec) = filter {
            self.base_filtered = self
                .base_filtered
                .iter()
                .copied()
                .filter(|&i| {
                    self.entry_rate(&self.index.entries[i])
                        .is_some_and(|r| matches_signed_cmp(r.round() as i64, spec))
                })
                .collect();
        }
        self.resort();
    }

    /// `base_filtered` を現在の並び順で `filtered` に反映し、先頭対局を読み込む。絞り込み
    /// 集合はそのままに並び順だけ変えるので、SFEN スキャン結果も並べ替えで維持される。
    fn resort(&mut self) {
        let mut filtered = self.base_filtered.clone();
        sort_filtered(&mut filtered, &self.index.entries, self.sort_mode, |e| {
            entry_date_key(&self.index, e)
        });
        self.filtered = filtered;
        self.selected = 0;
        self.load_selected();
    }

    /// live: 間隔経過時にソースのフィンガープリントを確認し、変化があれば索引を取り直す。
    /// 失敗はステータス表示に留めて操作を継続する(次の間隔で再試行)。
    fn maybe_live_reload(&mut self) {
        let Some(live) = &self.live else { return };
        if live.last_poll.elapsed() < live.interval {
            return;
        }
        let old_fp = live.fingerprint;
        if let Some(l) = self.live.as_mut() {
            l.last_poll = Instant::now();
        }
        let fp = match self.source.live_fingerprint() {
            Ok(Some(fp)) => fp,
            Ok(None) => return,
            Err(e) => {
                self.status = format!("live 再読込チェック失敗: {e}");
                return;
            }
        };
        if fp == old_fp {
            return;
        }
        if let Some(l) = self.live.as_mut() {
            l.fingerprint = fp;
        }
        match self.source.build_index() {
            Ok(new_index) => self.replace_index(new_index),
            Err(e) => self.status = format!("live 再読込失敗: {e}"),
        }
    }

    /// 索引を live 再読込の結果へ差し替える。選択中の対局は (出典パス, game_id) で追跡し、
    /// 見つかれば選択位置・表示中の手を維持する(完了局の内容は追記されない前提で、同一
    /// キーなら読み直さない)。
    fn replace_index(&mut self, new_index: GameIndex) {
        let old_len = self.index.entries.len();
        let selected_key = self.selected_entry().and_then(|e| entry_key(&self.index, e));
        let old_ply = self.selected_entry().map(|e| e.ply_count);
        let saved_game = self.current_game.take();
        let saved_move = self.current_move;
        // 末尾の手を見ていたなら、対局が伸びたとき新しい末尾へ追従する(観戦用)。
        // 追従モード(f)中は途中の手を見ていても常に新しい末尾へ。
        let was_at_tail = self.follow
            || saved_game
                .as_ref()
                .is_some_and(|g| !g.moves.is_empty() && saved_move + 1 == g.moves.len());

        self.index = new_index;
        // SFEN スキャンの一致集合は旧索引の添字なので新索引へは引き継げない。クリアして
        // 全件表示へ戻す(ステータスの件数表示で気づける)。
        if sfen_query_from_input(&self.filter_input).is_some() {
            self.filter_input.clear();
        }
        self.apply_filter();

        if let Some(key) = selected_key
            && let Some(pos) = self.filtered.iter().position(|&i| {
                entry_key(&self.index, &self.index.entries[i]).as_ref() == Some(&key)
            })
        {
            self.selected = pos;
            let new_ply = self.index.entries[self.filtered[pos]].ply_count;
            if old_ply == Some(new_ply) {
                // 内容が変わっていない(完了局)なら読み直さず手の位置を維持。
                self.current_game = saved_game;
                self.current_move = saved_move;
            } else {
                // 進行中対局が伸びた(live-mirror 経由等)。読み直して位置を復元し、
                // 末尾を見ていたなら新しい末尾へ追従する。
                self.load_selected();
                if let Some(game) = &self.current_game
                    && !game.moves.is_empty()
                {
                    self.current_move = if was_at_tail {
                        game.moves.len() - 1
                    } else {
                        saved_move.min(game.moves.len() - 1)
                    };
                }
            }
        }
        let new_len = self.index.entries.len();
        // 読み込みエラーが出ている(current_game が無い)ときは、その診断メッセージを
        // live 概況で上書きしない(盤面ペインが「ステータスバー参照」と誘導するため)。
        let load_failed = self.current_game.is_none() && !self.filtered.is_empty();
        if !load_failed {
            self.status = if new_len > old_len {
                format!("live: {} 局追加 (計 {new_len} 局)", new_len - old_len)
            } else {
                // 局数が変わらない再読込 = 進行中対局の追記など。「0 局追加」だと紛らわしい。
                format!("live: 更新 (計 {new_len} 局)")
            };
        }
    }

    fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.resort();
    }

    fn next_game(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
            self.load_selected();
        }
    }

    fn prev_game(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.load_selected();
        }
    }

    fn next_move(&mut self) {
        if let Some(game) = &self.current_game
            && self.current_move + 1 < game.moves.len()
        {
            self.current_move += 1;
        }
    }

    fn prev_move(&mut self) {
        if self.current_move > 0 {
            self.current_move -= 1;
        }
    }

    /// 最初の手（開始直後の局面）へジャンプする（`Home`/`g`）。
    fn first_move(&mut self) {
        self.current_move = 0;
    }

    /// 最終手へジャンプする（`End`/`G`）。live なら以降は暗黙の末尾追従に入る。
    fn last_move(&mut self) {
        if let Some(game) = &self.current_game
            && !game.moves.is_empty()
        {
            self.current_move = game.moves.len() - 1;
        }
    }

    /// ライブ追従モードのトグル（`f`）。live モード外では効かない。
    fn toggle_follow(&mut self) {
        if self.live.is_none() {
            self.status = "追従 (f) は --live 時のみ有効".to_string();
            return;
        }
        self.follow = !self.follow;
        if self.follow {
            self.last_move();
            self.status = "ライブ追従: 選択対局の最新手を表示（手を戻すと解除）".to_string();
        } else {
            self.status = "ライブ追従を解除".to_string();
        }
    }

    /// 末尾から離れる操作の後処理: 追従中なら解除する（追従が即座に末尾へ引き戻すのを防ぐ）。
    fn break_follow(&mut self) {
        if self.follow {
            self.follow = false;
            self.status = "ライブ追従を解除（f で再開）".to_string();
        }
    }

    /// ナビゲーション操作を実行し、表示位置が実際に動いたときのみ追従を解除する
    /// （末尾での `n` 空振り等、位置が変わらない操作で黙って解除しない）。
    fn nav_breaking_follow(&mut self, nav: impl FnOnce(&mut Self)) {
        let before = self.current_move;
        nav(self);
        if self.current_move != before {
            self.break_follow();
        }
    }

    fn jump_to_next_eval_swing(&mut self) {
        if let Some(game) = &self.current_game
            && let Some(idx) = next_eval_swing(game, self.current_move, EVAL_SWING_THRESHOLD_CP)
        {
            self.current_move = idx;
        }
    }

    fn jump_to_prev_eval_swing(&mut self) {
        if let Some(game) = &self.current_game
            && let Some(idx) = prev_eval_swing(game, self.current_move, EVAL_SWING_THRESHOLD_CP)
        {
            self.current_move = idx;
        }
    }

    /// `false` を返したらイベントループを終了する。
    fn handle_key(&mut self, code: KeyCode) -> bool {
        match self.mode {
            // ヘルプ表示中は何のキーでも閉じるだけ（`q` を押しても終了しない）。
            Mode::Help => self.mode = Mode::Browse,
            Mode::Filter => match code {
                KeyCode::Esc => {
                    self.filter_input.clear();
                    self.apply_filter();
                    self.mode = Mode::Browse;
                }
                KeyCode::Enter => {
                    // `sfen:` だけは即時フィルタではなく Enter で逐次スキャンを起動する。
                    // 値（SFEN 本体）は大文字＝先手・小文字＝後手で意味が変わるため、小文字化する
                    // `apply_filter` 経路ではなく生の `filter_input` から取り出す（プレフィクス
                    // 判定は他フィルタと揃えて大小無視）。
                    if let Some(sfen) = sfen_query_from_input(&self.filter_input) {
                        let sfen = sfen.to_string();
                        self.start_sfen_scan(&sfen);
                    }
                    self.mode = Mode::Browse;
                }
                KeyCode::Backspace => {
                    self.filter_input.pop();
                    self.apply_filter();
                }
                KeyCode::Char(c) => {
                    self.filter_input.push(c);
                    self.apply_filter();
                }
                _ => {}
            },
            Mode::Browse => match code {
                KeyCode::Char('q') | KeyCode::Esc => return false,
                KeyCode::Char('h') | KeyCode::Left => self.nav_breaking_follow(App::prev_move),
                KeyCode::Char('l') | KeyCode::Right => self.next_move(),
                KeyCode::Char('j') | KeyCode::Down => self.next_game(),
                KeyCode::Char('k') | KeyCode::Up => self.prev_game(),
                KeyCode::Home | KeyCode::Char('g') => self.nav_breaking_follow(App::first_move),
                KeyCode::End | KeyCode::Char('G') => self.last_move(),
                KeyCode::Char('f') => self.toggle_follow(),
                KeyCode::Char('n') => self.nav_breaking_follow(App::jump_to_next_eval_swing),
                KeyCode::Char('N') => self.nav_breaking_follow(App::jump_to_prev_eval_swing),
                KeyCode::Char('s') => self.cycle_sort_mode(),
                KeyCode::Char('/') => self.mode = Mode::Filter,
                KeyCode::Char('?') => self.mode = Mode::Help,
                _ => {}
            },
        }
        true
    }

    /// `sfen:` の値から逐次スキャンを開始する（Enter 契機）。フィールド数が SFEN として
    /// 不正な入力（3〜4 フィールド以外・空クエリ）は走査せず、その旨をステータスに出す。
    fn start_sfen_scan(&mut self, sfen: &str) {
        let Some(target) = parse_sfen_query(sfen) else {
            self.status =
                "SFEN 局面検索: SFEN が不正です（盤面 手番 持駒 [手数] の 3〜4 フィールド）"
                    .to_string();
            return;
        };
        self.scan = Some(SfenScan {
            target,
            next: 0,
            matches: Vec::new(),
        });
    }

    /// スキャンを `SFEN_SCAN_CHUNK` 対局ぶん進める。全件走査し終えたら結果を反映する。
    /// `load_game` に失敗した対局は照合対象から除外する（スキャンは止めない）。
    fn advance_sfen_scan(&mut self) {
        // `self.source`/`self.index` をループ内で借用するため scan を一旦取り出す。
        let Some(mut scan) = self.scan.take() else {
            return;
        };
        let total = self.index.entries.len();
        let end = (scan.next + SFEN_SCAN_CHUNK).min(total);
        for i in scan.next..end {
            let entry = &self.index.entries[i];
            if let Ok(game) = self.source.load_game(&self.index, entry)
                && game_contains_sfen(&game, &scan.target)
            {
                scan.matches.push(i);
            }
        }
        scan.next = end;
        if scan.next >= total {
            self.finish_sfen_scan(scan);
        } else {
            self.scan = Some(scan);
        }
    }

    /// スキャン完了：一致した対局だけを絞り込み集合にして現在の並び順で反映する。以後の
    /// 並べ替え（`s`）は `base_filtered` を再ソートするので、この結果は失われない。
    fn finish_sfen_scan(&mut self, scan: SfenScan) {
        let count = scan.matches.len();
        self.base_filtered = scan.matches;
        self.status.clear();
        self.resort();
        // `resort`→`load_selected` が先頭対局の読み込みエラーを status に残した場合は
        // 上書きせず、件数を前置きして両方見えるようにする。
        let headline = format!("SFEN 局面検索: {count} 件一致");
        self.status = if self.status.is_empty() {
            headline
        } else {
            format!("{headline}（{}）", self.status)
        };
    }

    /// 走査中のスキャンを中断する（`filtered` は開始前のまま）。
    fn cancel_sfen_scan(&mut self) {
        if self.scan.take().is_some() {
            self.status = "SFEN 局面検索を中断しました".to_string();
        }
    }
}

fn outcome_keyword(entry: &GameIndexEntry) -> &'static str {
    if entry.error {
        return "error";
    }
    match entry.outcome {
        Some(GameOutcomeView::Win(Color::Black)) => "black_win",
        Some(GameOutcomeView::Win(Color::White)) => "white_win",
        Some(GameOutcomeView::Draw) => "draw",
        None => "unknown",
    }
}

fn jsonl_game_id(entry: &GameIndexEntry) -> Option<u32> {
    match entry.source {
        GameSourceRef::Jsonl { game_id, .. } => Some(game_id),
        // CSA は 1 ファイル = 1 対局で `game_id` を持たない（番号検索は ordinal ベースで
        // 別途扱う）。
        GameSourceRef::Psv { .. } | GameSourceRef::Csa { .. } => None,
    }
}

/// 対局の出典ファイルメタ（PSV はファイル単位のメタを持たないため `None`）。
fn file_meta<'a>(index: &'a GameIndex, entry: &GameIndexEntry) -> Option<&'a PairFileMeta> {
    match entry.source {
        GameSourceRef::Jsonl { file_idx, .. } | GameSourceRef::Csa { file_idx, .. } => {
            index.pair_file(file_idx)
        }
        GameSourceRef::Psv { .. } => None,
    }
}

fn entry_date_key(index: &GameIndex, entry: &GameIndexEntry) -> Option<u64> {
    file_meta(index, entry).and_then(|m| m.date_key)
}

/// live 再読込を跨いで同一対局を同定するキー（出典パス + ファイル内 game_id）。
/// 再読込で `file_idx` や ordinal は振り直されるため添字では追跡できない。
fn entry_key(index: &GameIndex, entry: &GameIndexEntry) -> Option<(PathBuf, Option<u32>)> {
    file_meta(index, entry).map(|m| (m.path.clone(), jsonl_game_id(entry)))
}

/// 両対局者のレートから対局の代表レート（高い方）を出す。
fn combined_rate(black: Option<f64>, white: Option<f64>) -> Option<f64> {
    match (black, white) {
        (Some(b), Some(w)) => Some(b.max(w)),
        (b, None) => b,
        (None, w) => w,
    }
}

/// `/` 検索クエリを解析した結果。判定ロジックは `entry_matches` に集約する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter<'a> {
    Empty,
    /// `pair:4` 等の `field:value` 構文。指定フィールドの完全一致・比較・別名判定を見る。
    Field(FieldKind, &'a str),
    /// `reversal`/`decisive` 等、値を取らない述語。
    Keyword(KeywordKind),
    /// prefix 無しで数字のみのクエリ。ラベル部分一致を無効化し、数値フィールドの
    /// 完全一致のみを見る（`vol4B_raw` のようにラベルに数字を含むデータで
    /// `pair_index=4` のつもりの `"4"` がラベルにも部分一致してしまう問題への対応）。
    NumericExact(&'a str),
    /// それ以外の自由文字列。従来どおりラベル部分一致 OR outcome キーワード部分一致。
    Text(&'a str),
    /// `sfen:<SFEN>`。局面本体（各手の着手前 SFEN）が要るため即時フィルタでは絞り込まず
    /// （`entry_matches` で常に一致扱い）、Enter で全対局を逐次スキャンして反映する。
    Sfen(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Pair,
    Slot,
    Startpos,
    Id,
    Outcome,
    Label,
    /// `winner:sente|gote`（`black|white` も可）。勝者側で絞り込む。
    Winner,
    /// `len:>N|<N|N`。手数で絞り込む。
    Len,
    /// `swing:>N|<N|N`。評価値振れ幅（`max_swing_cp`）で絞り込む。
    Swing,
    /// `rate:>N|<N|N`。対局の代表レート（両対局者の高い方）で絞り込む。判定はレート表を
    /// 持つ `App::apply_filter` 側で行う（`entry_matches` では常に一致扱い）。
    Rate,
    /// `date:YYYYMMDD`。ファイル名由来の対局日時キーへの前方一致（`date:202607` 等の
    /// 部分指定も可）。
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeywordKind {
    /// 両者が優勢になった局面がある（形勢逆転棋譜）。
    Reversal,
    /// 勝敗が付いた（引き分け・エラー・不明でない）。
    Decisive,
}

/// 検索クエリを解析する。`query` は呼び出し側で既に小文字化済みの前提。
fn parse_filter(query: &str) -> Filter<'_> {
    if query.is_empty() {
        return Filter::Empty;
    }
    if let Some((prefix, value)) = query.split_once(':') {
        // SFEN は局面本体が要るので即時フィルタ経路に乗せず、Enter で逐次スキャンして扱う。
        if prefix == "sfen" {
            return Filter::Sfen(value);
        }
        let field = match prefix {
            "pair" => Some(FieldKind::Pair),
            "slot" => Some(FieldKind::Slot),
            "startpos" => Some(FieldKind::Startpos),
            "id" => Some(FieldKind::Id),
            "outcome" => Some(FieldKind::Outcome),
            "label" => Some(FieldKind::Label),
            "winner" => Some(FieldKind::Winner),
            "len" => Some(FieldKind::Len),
            "swing" => Some(FieldKind::Swing),
            "rate" => Some(FieldKind::Rate),
            "date" => Some(FieldKind::Date),
            _ => None,
        };
        if let Some(field) = field {
            return Filter::Field(field, value);
        }
    }
    match query {
        "reversal" => return Filter::Keyword(KeywordKind::Reversal),
        "decisive" => return Filter::Keyword(KeywordKind::Decisive),
        _ => {}
    }
    if query.bytes().all(|b| b.is_ascii_digit()) {
        return Filter::NumericExact(query);
    }
    Filter::Text(query)
}

/// `>N` / `<N` / `N` の比較指定に `actual` が一致するか。パースできない指定は不一致。
fn matches_numeric_cmp(actual: u32, spec: &str) -> bool {
    if let Some(n) = spec.strip_prefix('>') {
        n.parse::<u32>().is_ok_and(|n| actual > n)
    } else if let Some(n) = spec.strip_prefix('<') {
        n.parse::<u32>().is_ok_and(|n| actual < n)
    } else {
        spec.parse::<u32>().is_ok_and(|n| actual == n)
    }
}

/// [`matches_numeric_cmp`] の符号付き版。負値がありうる指標(floodgate レート等)用で、
/// `rate:>-100` / `rate:<-500` のような負の閾値も受け付ける。
fn matches_signed_cmp(actual: i64, spec: &str) -> bool {
    if let Some(n) = spec.strip_prefix('>') {
        n.parse::<i64>().is_ok_and(|n| actual > n)
    } else if let Some(n) = spec.strip_prefix('<') {
        n.parse::<i64>().is_ok_and(|n| actual < n)
    } else {
        spec.parse::<i64>().is_ok_and(|n| actual == n)
    }
}

fn entry_matches(index: &GameIndex, entry: &GameIndexEntry, filter: Filter<'_>) -> bool {
    match filter {
        Filter::Empty => true,
        Filter::Field(FieldKind::Pair, v) => entry.pair_index.is_some_and(|x| x.to_string() == v),
        Filter::Field(FieldKind::Slot, v) => entry.pair_slot.is_some_and(|x| x.to_string() == v),
        Filter::Field(FieldKind::Startpos, v) => {
            entry.startpos_idx.is_some_and(|x| x.to_string() == v)
        }
        Filter::Field(FieldKind::Id, v) => jsonl_game_id(entry).is_some_and(|x| x.to_string() == v),
        Filter::Field(FieldKind::Outcome, v) => outcome_keyword(entry).contains(v),
        Filter::Field(FieldKind::Label, v) => {
            display_label(index, entry).to_lowercase().contains(v)
        }
        Filter::Field(FieldKind::Winner, v) => match entry.outcome {
            Some(GameOutcomeView::Win(Color::Black)) => v == "sente" || v == "black",
            Some(GameOutcomeView::Win(Color::White)) => v == "gote" || v == "white",
            _ => false,
        },
        Filter::Field(FieldKind::Len, v) => matches_numeric_cmp(entry.ply_count, v),
        Filter::Field(FieldKind::Swing, v) => {
            entry.metrics.max_swing_cp.is_some_and(|s| matches_numeric_cmp(s, v))
        }
        // レート表は App が持つため即時フィルタでは絞らない（`apply_filter` で絞る）。
        Filter::Field(FieldKind::Rate, _) => true,
        Filter::Field(FieldKind::Date, v) => {
            entry_date_key(index, entry).is_some_and(|k| k.to_string().starts_with(v))
        }
        Filter::Keyword(KeywordKind::Reversal) => entry.metrics.had_reversal(REVERSAL_THRESHOLD_CP),
        Filter::Keyword(KeywordKind::Decisive) => {
            !entry.error && matches!(entry.outcome, Some(GameOutcomeView::Win(_)))
        }
        Filter::NumericExact(v) => [
            entry.pair_index,
            entry.pair_slot,
            entry.startpos_idx,
            jsonl_game_id(entry),
        ]
        .iter()
        .flatten()
        .any(|x| x.to_string() == v),
        Filter::Text(v) => {
            display_label(index, entry).to_lowercase().contains(v)
                || outcome_keyword(entry).contains(v)
        }
        // 局面本体は索引に無いので即時フィルタでは絞らない（Enter で逐次スキャン）。
        Filter::Sfen(_) => true,
    }
}

/// SFEN 局面検索の比較キー。末尾の手数フィールド（4 個目）を落とし、「盤面・手番・持駒」の
/// 3 フィールドで比較する。同一局面でも手数カウンタは対局中の位置で変わるため無視する。
/// 大文字＝先手駒・小文字＝後手駒で意味が変わるので、`/` フィルタと違い小文字化はしない。
fn normalize_sfen(s: &str) -> String {
    s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

/// ユーザー入力の SFEN クエリを検証し、比較キー（`normalize_sfen` と同じ 3 フィールド）を返す。
/// 受け付けるのは「盤面 手番 持駒」の 3 フィールド、または末尾に手数を足した 4 フィールド
/// （手数は数値、比較では無視）のみ。それ以外（フィールド不足・余剰・手数が非数値）は `None`
/// を返し、呼び出し側で走査せずエラー表示する（無駄な全走査と余剰フィールドの誤ヒットを防ぐ）。
fn parse_sfen_query(s: &str) -> Option<String> {
    let fields: Vec<&str> = s.split_whitespace().collect();
    match fields.as_slice() {
        [board, side, hands] => Some(format!("{board} {side} {hands}")),
        [board, side, hands, moveno] if moveno.bytes().all(|b| b.is_ascii_digit()) => {
            Some(format!("{board} {side} {hands}"))
        }
        _ => None,
    }
}

/// `filter_input` が（プレフィクスのみ大小無視で）`sfen:` なら、値部分（原文＝大小保持）を返す。
/// 即時フィルタは小文字化後に `sfen` を認識するので、Enter 経路とステータス表示もプレフィクス
/// だけは同様に大小無視にして揃える（`SFEN:` で即時表示は絞られないのに Enter が無反応になる
/// 不一致を防ぐ）。値の SFEN 本体は大小に意味があるので原文のまま返す。
fn sfen_query_from_input(input: &str) -> Option<&str> {
    let prefix = "sfen:";
    match input.get(..prefix.len()) {
        Some(head) if head.eq_ignore_ascii_case(prefix) => input.get(prefix.len()..),
        _ => None,
    }
}

/// 対局中のいずれかの着手前局面が `target`（正規化済み SFEN）と一致するか。
fn game_contains_sfen(game: &GameRecord, target: &str) -> bool {
    game.moves.iter().any(|mv| normalize_sfen(&mv.sfen_before) == target)
}

fn outcome_sort_key(entry: &GameIndexEntry) -> u8 {
    if entry.error {
        return 0;
    }
    match entry.outcome {
        Some(GameOutcomeView::Win(Color::Black)) => 1,
        Some(GameOutcomeView::Win(Color::White)) => 2,
        Some(GameOutcomeView::Draw) => 3,
        None => 4,
    }
}

/// `filtered`（`index.entries` への index 列）を `mode` に従って安定ソートする。
/// 安定ソートなので、同一キー内の相対順は呼び出し前の順序（発見順）を維持する。
/// `date_key` は対局の日時キーを引く closure（`SortMode::Date` のみ使用。テストからは
/// 索引を組み立てずに注入できる）。
fn sort_filtered(
    filtered: &mut [usize],
    entries: &[GameIndexEntry],
    mode: SortMode,
    date_key: impl Fn(&GameIndexEntry) -> Option<u64>,
) {
    use std::cmp::Reverse;
    // 指標ソートは降順。評価値の無い対局（None）は末尾へ寄せる（`is_none` を第 1 キーに）。
    match mode {
        SortMode::Discovery => {}
        SortMode::Date => filtered.sort_by_key(|&i| {
            let k = date_key(&entries[i]);
            (k.is_none(), Reverse(k.unwrap_or(0)))
        }),
        SortMode::Outcome => filtered.sort_by_key(|&i| outcome_sort_key(&entries[i])),
        SortMode::Length => filtered.sort_by_key(|&i| Reverse(entries[i].ply_count)),
        SortMode::Decisiveness => filtered.sort_by_key(|&i| {
            let m = entries[i].metrics.final_cp;
            (m.is_none(), Reverse(m.map(|c| c.unsigned_abs()).unwrap_or(0)))
        }),
        SortMode::Swing => filtered.sort_by_key(|&i| {
            let m = entries[i].metrics.max_swing_cp;
            (m.is_none(), Reverse(m.unwrap_or(0)))
        }),
    }
}

/// live 有効時の入力待ちの上限。この間隔で入力待ちを切り上げて再読込チェックへ回る
/// （実際に fingerprint を見るかは `LiveState::interval` が制御する）。
const LIVE_POLL_TICK: Duration = Duration::from_millis(250);

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if app.scan.is_some() {
            // 逐次スキャン中はブロックせず、キー入力があれば中断だけ受け付け、無ければ
            // 1 tick ぶん走査を進める。draw を挟むので進捗がステータスバーに反映される。
            if event::poll(Duration::ZERO)? {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                {
                    app.cancel_sfen_scan();
                }
            } else {
                app.advance_sfen_scan();
            }
        } else if app.live.is_some() {
            // live 中はブロック読みだと新規対局に気づけないため、短い tick で入力待ちを
            // 切り上げて再読込チェックを回す。
            if event::poll(LIVE_POLL_TICK)? {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                    && !app.handle_key(key.code)
                {
                    return Ok(());
                }
            } else {
                app.maybe_live_reload();
            }
        } else if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && !app.handle_key(key.code)
        {
            return Ok(());
        }
    }
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // 盤面は罫線＋筋ラベル込みで25行の本文＋Blockの上下枠2行=27行を必要とする。
            Constraint::Min(27),
            Constraint::Length(9),
            Constraint::Length(3),
        ])
        .split(frame.area());

    // 盤面は内容ぴったりの固定幅（`BOARD_PANEL_WIDTH`）にし、余りは指し手パネルへ回す
    // （盤面を広い割合で確保すると右側が広大なデッドゾーンになるため）。
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Length(BOARD_PANEL_WIDTH),
            Constraint::Min(40),
        ])
        .split(root[0]);

    draw_game_list(frame, app, main[0]);
    draw_board(frame, app, main[1]);
    draw_move_list(frame, app, main[2]);
    draw_eval_graph(frame, app, root[1]);
    draw_status_bar(frame, app, root[2]);

    if matches!(app.mode, Mode::Help) {
        draw_help_popup(frame, frame.area());
    }
}

fn draw_game_list(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .map(|&i| {
            let entry = &app.index.entries[i];
            let label = display_label(&app.index, entry);
            let marker = if entry.error {
                " [error]"
            } else {
                match entry.outcome {
                    Some(GameOutcomeView::Win(Color::Black)) => " [b-win]",
                    Some(GameOutcomeView::Win(Color::White)) => " [w-win]",
                    Some(GameOutcomeView::Draw) => " [draw]",
                    None => "",
                }
            };
            // 終局が記録されていない対局（live では進行中）を左端の `>` + 緑で区別する。
            // 曖昧幅の記号（▶ 等）は端末設定次第で列ずれするため ASCII を使う。
            let in_progress = !entry.error && entry.outcome.is_none();
            let prefix = if in_progress { "> " } else { "  " };
            let item = ListItem::new(format!("{prefix}{label}{marker}{}", rate_suffix(app, entry)));
            if in_progress {
                item.style(Style::default().fg(RColor::Green))
            } else {
                item
            }
        })
        .collect();

    let live_mark = if app.live.is_some() {
        if app.follow {
            " [live 追従]"
        } else {
            " [live]"
        }
    } else {
        ""
    };
    let title = format!(
        "対局一覧 ({}/{}) [{}]{live_mark}",
        app.filtered.len(),
        app.index.entries.len(),
        app.sort_mode.label()
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if !app.filtered.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// `--ratings` 供給時のみ、対局者のレートを ` R3706/3512`（先手/後手）形式で付ける。
/// 片方だけ引けたら引けない側は `-`、両方引けなければ空文字。
fn rate_suffix(app: &App, entry: &GameIndexEntry) -> String {
    if app.ratings.is_empty() {
        return String::new();
    }
    let Some(meta) = file_meta(&app.index, entry) else {
        return String::new();
    };
    let black = app.rate_of(&meta.black_label);
    let white = app.rate_of(&meta.white_label);
    if black.is_none() && white.is_none() {
        return String::new();
    }
    let fmt = |r: Option<f64>| {
        r.map(|r| format!("{}", r.round() as i64)).unwrap_or_else(|| "-".to_string())
    };
    format!(" R{}/{}", fmt(black), fmt(white))
}

/// 対局・盤面・指し手ペインが「表示できる手が無い」ときに、その理由を区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyState {
    NoSelection,
    LoadFailed,
    ErrorGame,
    EmptyGame,
}

fn empty_state(
    selected_entry: Option<&GameIndexEntry>,
    status: &str,
    current_game: Option<&GameRecord>,
) -> Option<EmptyState> {
    let Some(entry) = selected_entry else {
        return Some(EmptyState::NoSelection);
    };
    if !status.is_empty() {
        return Some(EmptyState::LoadFailed);
    }
    match current_game {
        Some(game) if game.moves.is_empty() => {
            if entry.error {
                Some(EmptyState::ErrorGame)
            } else {
                Some(EmptyState::EmptyGame)
            }
        }
        Some(_) => None,
        None => Some(EmptyState::NoSelection),
    }
}

fn empty_state_message(state: EmptyState) -> &'static str {
    match state {
        EmptyState::NoSelection => "(対局を選択してください)",
        EmptyState::LoadFailed => "(対局の読み込みに失敗しました。ステータスバー参照)",
        EmptyState::ErrorGame => "エラー対局（対局データなし）",
        EmptyState::EmptyGame => "(0手の対局：指し手がありません)",
    }
}

fn empty_state_text(app: &App) -> &'static str {
    empty_state(app.selected_entry(), &app.status, app.current_game.as_ref())
        .map(empty_state_message)
        .unwrap_or("(対局を選択してください)")
}

fn draw_board(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let lines = match current_move(app) {
        Some(mv) => render_board(&mv.sfen_before, mv.mv, mv.annotation.timed_out.unwrap_or(false)),
        None => vec![Line::from(empty_state_text(app))],
    };
    let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("盤面"));
    frame.render_widget(para, area);
}

fn draw_move_list(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = match &app.current_game {
        Some(game) if !game.moves.is_empty() => game
            .moves
            .iter()
            .enumerate()
            .map(|(i, mv)| move_list_item(game, i, mv))
            .collect(),
        _ => vec![ListItem::new(empty_state_text(app))],
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("指し手"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if app.current_game.as_ref().is_some_and(|g| !g.moves.is_empty()) {
        state.select(Some(app.current_move));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn move_list_item(game: &GameRecord, i: usize, mv: &MoveView) -> ListItem<'static> {
    let mut spans = Vec::new();
    if let Some(skipped) = ply_gap_before(game, i) {
        spans.push(Span::styled(
            format!("⋯{skipped}手欠落⋯ "),
            Style::default().fg(RColor::DarkGray),
        ));
    }
    spans.push(Span::raw(mv.kif_label.clone()));
    let annotation = annotation_inline(mv);
    if !annotation.is_empty() {
        // 注釈は補助情報なので淡色で並べる。1行に収まらなければ幅で自然に切れる。
        spans.push(Span::styled(format!("  {annotation}"), Style::default().fg(RColor::DarkGray)));
    }
    ListItem::new(Line::from(spans))
}

/// index `i` の手の直前に手数の欠番があれば、欠落した手数を返す。
/// PSV の `skip_initial_ply`/`skip_in_check` によるレコード欠番を可視化する用途。
///
/// `i == 0`（先頭欠番）は `game.leading_gap_is_drop` が true のときだけ検出する。
/// JSONL は定跡途中開始で先頭手数が 1 超になりうるが、それは欠落ではないため
/// false を設定しており、先頭マーカーは出さない（`game.moves` は呼び出し元
/// `move_list_item` で非空を確認済みなので `game.moves[0]` へのアクセスは安全）。
fn ply_gap_before(game: &GameRecord, i: usize) -> Option<u32> {
    if i == 0 {
        if !game.leading_gap_is_drop {
            return None;
        }
        let first_ply = game.moves[0].ply;
        return (first_ply > 1).then(|| first_ply - 1);
    }
    let prev_ply = game.moves[i - 1].ply;
    let cur_ply = game.moves[i].ply;
    // `then_some` は引数を先行評価するため、条件が false のときも
    // `cur_ply - prev_ply - 1` が評価されて u32 アンダーフローしうる。
    // 遅延評価の `then` でガードする。
    (cur_ply > prev_ply + 1).then(|| cur_ply - prev_ply - 1)
}

/// 評価値グラフの Y 軸クランプ幅（cp 換算）。詰みはこの符号付き値に丸める。
const GRAPH_CP_CLAMP: f64 = 3000.0;

/// 「評価値が大きく動いた手」とみなす |Δcp| の閾値。歩2枚分の評価値変動を目安にした固定値。
const EVAL_SWING_THRESHOLD_CP: f64 = 200.0;

/// `reversal` 検索の閾値。両者がこの cp 以上優勢になった局面があれば形勢逆転とみなす。
const REVERSAL_THRESHOLD_CP: i32 = 300;

/// 手番相対の生スコアから、先手固定 POV の打点値を導出する。
/// プラス = 先手優勢、マイナス = 後手優勢（design doc「評価値グラフ」節参照）。
/// `score_cp`/`score_mate` が両方とも無い手は `None`（打点をスキップする）。
fn black_pov_cp(mv: &MoveView) -> Option<f64> {
    let a = &mv.annotation;
    let stm_relative = if let Some(mate) = a.score_mate {
        if mate >= 0 {
            GRAPH_CP_CLAMP
        } else {
            -GRAPH_CP_CLAMP
        }
    } else {
        a.score_cp? as f64
    };
    let black_pov = if mv.side == Color::Black {
        stm_relative
    } else {
        -stm_relative
    };
    Some(black_pov.clamp(-GRAPH_CP_CLAMP, GRAPH_CP_CLAMP))
}

/// `game.moves` と同じ長さ・同じ並びの打点列（評価値が無い手は `None`）。評価値付きの手の
/// 元インデックスを保つため flat にしない（`evaluated_points` が index 付きで使う）。
fn eval_points(game: &GameRecord) -> Vec<Option<(f64, f64, Color)>> {
    game.moves
        .iter()
        .map(|mv| black_pov_cp(mv).map(|cp| (mv.ply as f64, cp, mv.side)))
        .collect()
}

/// `game.moves` を評価値付きの手だけに絞り、`(元の手の index, 先手 POV cp)` の列にする。
fn evaluated_points(game: &GameRecord) -> Vec<(usize, f64)> {
    eval_points(game)
        .into_iter()
        .enumerate()
        .filter_map(|(i, p)| p.map(|(_, cp, _)| (i, cp)))
        .collect()
}

/// `from` より後ろの手のうち、直前の評価値付きの手との |Δcp| が `threshold` を
/// 超える最初の手の index。
fn next_eval_swing(game: &GameRecord, from: usize, threshold: f64) -> Option<usize> {
    let points = evaluated_points(game);
    points
        .windows(2)
        .find(|w| w[1].0 > from && (w[1].1 - w[0].1).abs() > threshold)
        .map(|w| w[1].0)
}

/// `from` より前の手のうち、直前の評価値付きの手との |Δcp| が `threshold` を
/// 超える直近の手の index。
fn prev_eval_swing(game: &GameRecord, from: usize, threshold: f64) -> Option<usize> {
    let points = evaluated_points(game);
    points
        .windows(2)
        .rev()
        .find(|w| w[1].0 < from && (w[1].1 - w[0].1).abs() > threshold)
        .map(|w| w[1].0)
}

fn draw_eval_graph(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("評価値グラフ（＋先手優勢／－後手優勢）");
    let Some(game) = &app.current_game else {
        frame.render_widget(Paragraph::new("(対局を選択してください)").block(block), area);
        return;
    };
    let plotted: Vec<(f64, f64, Color)> = eval_points(game).into_iter().flatten().collect();
    if plotted.len() < 2 {
        frame.render_widget(Paragraph::new("(表示できる評価値がありません)").block(block), area);
        return;
    }

    // x_bounds は「評価値がある手」ではなく対局全体の ply 範囲に合わせる。
    // plotted 基準にすると、先頭 N 手が eval=None の対局で current_move が
    // その範囲にあるとき cursor_ply < min_ply になり、カーソル縦線が
    // Canvas のクリップで描画されなくなる。
    let min_ply = game.moves.first().map(|mv| mv.ply as f64).unwrap_or(0.0);
    let max_ply = game.moves.last().map(|mv| mv.ply as f64).unwrap_or(1.0).max(min_ply + 1.0);
    let cursor_ply = current_move(app).map(|mv| mv.ply as f64);

    let canvas = Canvas::default()
        .block(block)
        .x_bounds([min_ply, max_ply])
        .y_bounds([-GRAPH_CP_CLAMP * 1.1, GRAPH_CP_CLAMP * 1.1])
        .paint(move |ctx| {
            // 0 の水平基準線。
            ctx.draw(&CanvasLine {
                x1: min_ply,
                y1: 0.0,
                x2: max_ply,
                y2: 0.0,
                color: RColor::DarkGray,
            });
            // 評価値付きの手を出現順に結ぶ（着手後の評価値を、その着手側の色で）。CSA のように
            // 片側エンジンしか評価値を書かない棋譜では評価値が 1 手おきになり、「手インデックスで
            // 隣接」を結ぶと隣り合う評価値付きの手が無く線が 1 本も引けないため、評価値付きの手の
            // 並びで隣接を結ぶ（X 軸は ply なので欠けた手の区間はそのぶん横に広い線分になる）。
            for pair in plotted.windows(2) {
                let (x1, y1, _) = pair[0];
                let (x2, y2, side2) = pair[1];
                let color = if side2 == Color::Black {
                    RColor::Yellow
                } else {
                    RColor::Cyan
                };
                ctx.draw(&CanvasLine {
                    x1,
                    y1,
                    x2,
                    y2,
                    color,
                });
            }
            if let Some(cursor) = cursor_ply {
                ctx.draw(&CanvasLine {
                    x1: cursor,
                    y1: -GRAPH_CP_CLAMP * 1.1,
                    x2: cursor,
                    y2: GRAPH_CP_CLAMP * 1.1,
                    color: RColor::White,
                });
            }
        });
    frame.render_widget(canvas, area);
}

fn draw_status_bar(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    // 逐次スキャン中は他のどのモード表示よりも進捗を優先する。
    if let Some(scan) = &app.scan {
        let text = format!(
            "SFEN 局面検索中 {}/{}（一致 {} 件）  Esc/q:中断",
            scan.next,
            app.index.entries.len(),
            scan.matches.len()
        );
        let para = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
        frame.render_widget(para, area);
        return;
    }
    let text = match &app.mode {
        Mode::Filter if sfen_query_from_input(&app.filter_input).is_some() => format!(
            "SFEN 局面検索: {}_   （Enter で全 {} 対局を走査 / 手数は無視）",
            app.filter_input,
            app.index.entries.len()
        ),
        Mode::Filter => format!(
            "検索 [id: pair: outcome: winner:sente|gote len:>N swing:>N rate:>N date: reversal sfen: label:]: {}_   （一致 {}件）",
            app.filter_input,
            app.filtered.len()
        ),
        Mode::Help => "何かキーを押すとヘルプを閉じます".to_string(),
        Mode::Browse => {
            // 通常時はヘルプを行頭に固定する（手を動かしても位置がずれないよう、可変長の
            // 注釈はここに出さず指し手パネル側へ移した）。エラー等の status がある時は、
            // 長いヘルプで末尾 truncate されて隠れないよう status を先頭に置く（優先情報）。
            // 現在の並び順は対局一覧のタイトルに出す（可変長ラベルをここに出すと
            // 並び替えのたびにレイアウトシフトする）。
            let follow_hint = if app.live.is_some() { "  f:追従" } else { "" };
            let help = format!(
                "h/l:手  Home/End:先頭/末尾  j/k:対局  n/N:評価値急変  s:並替{follow_hint}  /:検索  ?:ヘルプ  q:終了"
            );
            if app.status.is_empty() {
                format!("[{help}]")
            } else {
                format!("{}   [{help}]", app.status)
            }
        }
    };
    let para = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, area);
}

fn draw_help_popup(frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    let popup_area = centered_rect(64, 70, area);
    frame.render_widget(Clear, popup_area);
    let lines = vec![
        Line::from("h / ←    1手戻す"),
        Line::from("l / →    1手進める"),
        Line::from("j / ↓    次の対局（フィルタ後のリスト内）"),
        Line::from("k / ↑    前の対局"),
        Line::from("Home / g 最初の手へジャンプ"),
        Line::from("End / G  最終手へジャンプ"),
        Line::from(
            "f        ライブ追従の切り替え（--live 時のみ。選択対局の最新手を表示し続ける）",
        ),
        Line::from("n        次の評価値急変手へジャンプ"),
        Line::from("N        前の評価値急変手へジャンプ"),
        Line::from(format!(
            "s        対局リストの並べ替えを切り替え（{}/{}/{}/{}/{}/{}）",
            SortMode::Discovery.label(),
            SortMode::Date.label(),
            SortMode::Outcome.label(),
            SortMode::Length.label(),
            SortMode::Decisiveness.label(),
            SortMode::Swing.label()
        )),
        Line::from("/        検索・フィルタ入力（Enter/Esc で終了、Esc はクリアも兼ねる）"),
        Line::from("?        このヘルプの表示・終了"),
        Line::from("q / Esc  終了（ヘルプ表示中は閉じるだけ）"),
        Line::from(""),
        Line::from("検索構文: pair:<n> slot:<n> startpos:<n> id:<n> outcome:<kw> label:<text>"),
        Line::from(
            "          winner:sente|gote  len:>N|<N|N  swing:>N  reversal  decisive|draw|error",
        ),
        Line::from("          rate:>N|<N|N  対局の代表レートで絞り込み（--ratings 供給時のみ）"),
        Line::from("          date:YYYYMMDD  ファイル名由来の対局日時へ前方一致（date:202607 等）"),
        Line::from(
            "          sfen:<SFEN>  局面本体を全対局走査（Enter で実行 / Esc・q で中断 / 手数は無視）",
        ),
        Line::from(
            "prefix 無しで数字のみを入力すると、id/pair/slot/startpos の完全一致で絞り込みます",
        ),
    ];
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("キーバインド一覧"));
    frame.render_widget(para, popup_area);
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn current_move(app: &App) -> Option<&MoveView> {
    app.current_game.as_ref().and_then(|g| g.moves.get(app.current_move))
}

/// 指し手行に並べる注釈（評価値・探索情報）。注釈が無い手は空文字を返す（行に何も足さない）。
/// engine 名や think limit は対局ラベルと重複・誤解を招くため `MoveAnnotation` に持たせない。
fn annotation_inline(mv: &MoveView) -> String {
    let a = &mv.annotation;
    let mut parts = Vec::new();
    // 評価値・詰みは手番相対（USI）で格納しているので、グラフと同じ先手基準（＋先手良し／
    // －後手良し）へ変換して表示する。手番相対のままだと手番交代のたびに符号が反転して読みにくい。
    let to_black_pov = |v: i32| if mv.side == Color::White { -v } else { v };
    if let Some(v) = a.score_mate {
        parts.push(format!("詰み{:+}", to_black_pov(v)));
    } else if let Some(v) = a.score_cp {
        parts.push(format!("評価値{:+}", to_black_pov(v)));
    }
    if let Some(v) = a.depth {
        parts.push(format!("depth={v}"));
    }
    if let Some(v) = a.seldepth {
        parts.push(format!("seldepth={v}"));
    }
    if let Some(v) = a.nodes {
        parts.push(format!("nodes={v}"));
    }
    if let Some(v) = a.nps {
        parts.push(format!("nps={v}"));
    }
    if let Some(v) = a.elapsed_ms {
        parts.push(format_elapsed(v));
    }
    if a.timed_out == Some(true) {
        parts.push("TIMEOUT".to_string());
    }
    parts.join(" ")
}

/// 消費時間の表示。秒単位で書く（CSA の `T` 行は元々秒粒度で、ms 表記だと常に 000 が
/// 付くだけの冗長表示になる）。ms 精度の値（JSONL 由来）は 0.1 秒単位へ整数演算で
/// 四捨五入する（浮動小数だと 0.05 秒境界の丸めが 2 進表現に依存するため）。
fn format_elapsed(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        let ds = (ms + 50) / 100;
        format!("{}.{}s", ds / 10, ds % 10)
    }
}

/// `mv` の着手元・着手先マス。駒打ちは着手元を持たない。パス等の通常手ではない
/// 指し手は両方 `None`（ハイライトしない）。
fn move_highlight_squares(mv: Move) -> (Option<Square>, Option<Square>) {
    if !mv.is_normal() {
        return (None, None);
    }
    let to = mv.to();
    if mv.is_drop() {
        (None, Some(to))
    } else {
        (Some(mv.from()), Some(to))
    }
}

/// 盤面1マスぶんの表示幅（カラム数）。全角駒(2カラム)を左右均等に中央寄せするため
/// 偶数にする。罫線の横棒・座標ラベルもこの幅に揃える。9×9 の全格子だと高さは19行で
/// 固定なので、正方形寄りに見せるにはこの幅で横方向に広げて縦横比を調整する。
const CELL_WIDTH: usize = 4;

/// 盤面パネルの確保幅（カラム数）。段ラベル付きの駒行が最大幅で、
/// 左枠 `│` 1 ＋ 9×(`CELL_WIDTH`＋区切り `│` 1) ＋ 段ラベル " 段" 3 ＋ Block 枠 2、
/// これに余白 1 を足す。`CELL_WIDTH` から算出するので幅を変えても追従する。
const BOARD_PANEL_WIDTH: u16 = 1 + 9 * (CELL_WIDTH as u16 + 1) + 3 + 2 + 1;

/// 最終手ハイライトの背景色（中明度の緑）。前景（駒の先後色）を残したまま、指し終えた
/// 手の移動先（駒）と移動元（空マス）の両方をこの色で示す。中間の明度なので暗い端末でも
/// 明るい端末でも背景から浮き、黄/シアンの駒色とも両立する。
const LAST_MOVE_BG: RColor = RColor::Rgb(58, 125, 70);

/// 盤面上端の筋ラベル（左＝９筋 … 右＝１筋、全角）。
const FILE_LABELS: [&str; 9] = ["９", "８", "７", "６", "５", "４", "３", "２", "１"];

/// 盤面右端の段ラベル（上＝一段 … 下＝九段）。
const RANK_LABELS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];

/// 盤面表示用の一文字グリフ。盤上の駒種（成り駒は `Pro*`）を受け取る。
///
/// 指し手・持駒パネルは綴り表記の `piece_label`（成香/成桂/成銀）を使うが、盤面は
/// 罫線と揃えるため成り駒も全角一文字で表す（成香→杏 / 成桂→圭 / 成銀→全）。これで
/// と/馬/龍 も含め全駒が全角一文字(2カラム)になり、`center_cell` で均等に中央寄せできる。
fn board_glyph(piece_type: PieceType) -> &'static str {
    match piece_type {
        PieceType::ProLance => "杏",
        PieceType::ProKnight => "圭",
        PieceType::ProSilver => "全",
        _ => piece_label(piece_type, piece_type.is_promoted()),
    }
}

/// 全角駒グリフ(2カラム)を `CELL_WIDTH` カラムのマスに中央寄せする。余白は半角スペース
/// (U+0020)で埋める：半角スペースは環境非依存で必ず1カラムなので、全角文字と混在しても
/// 列がズレない。`CELL_WIDTH` が偶数なら左右対称に揃う。
fn center_cell(glyph: &str) -> String {
    let pad = CELL_WIDTH.saturating_sub(2);
    let left = pad / 2;
    let right = pad - left;
    format!("{}{glyph}{}", " ".repeat(left), " ".repeat(right))
}

/// 空マス（`CELL_WIDTH` ぶんの半角スペース）。
fn empty_cell() -> String {
    " ".repeat(CELL_WIDTH)
}

/// 上端の筋ラベル行。各筋の数字を罫線のマス位置に中央寄せで並べる。
fn file_label_line() -> String {
    let mut s = String::from(" "); // 左枠（┌）のカラムぶん
    for label in FILE_LABELS {
        s.push_str(&center_cell(label));
        s.push(' '); // 縦罫線（┬／│）のカラムぶん
    }
    s
}

/// 罫線の1行ぶん（`left`/`mid`/`right` は角・交点の文字）。
fn horizontal_border(left: char, mid: char, right: char) -> String {
    let segment = "─".repeat(CELL_WIDTH);
    let mut s = String::new();
    s.push(left);
    for i in 0..9 {
        s.push_str(&segment);
        s.push(if i < 8 { mid } else { right });
    }
    s
}

fn render_board(sfen: &str, mv: Move, timed_out: bool) -> Vec<Line<'static>> {
    let mut pos = Position::new();
    if pos.set_sfen(sfen).is_err() {
        return vec![Line::from("(局面を表示できません)")];
    }

    // `sfen` は着手前の局面。実際に指された通常手・駒打ちだけを適用して指了後の局面を
    // 表示し、移動先（駒）と移動元（空マス）をハイライトする。手番・王手・持駒も適用後を
    // 反映する（王手表示＝最終手が王手だったこと）。
    //
    // 適用しないケース（記録局面＝指了前のまま表示）:
    // - pass 手・終局擬似手（is_normal=false）: pass 権 state を持たない局面で
    //   do_pass_move が panic するため、そもそも適用しない。
    // - タイムアウト行（timed_out）: 記録側はタイムアウトでも bestmove を move_usi に
    //   残すが局面には適用せず終局する。この bestmove は実際には指されておらず、遅れて
    //   返った手だと非合法で do_move が panic しうるため適用しない。
    // これらを除いた「適用する mv」は、記録側が実際に指した合法手なので do_move は安全。
    let apply = mv.is_normal() && !timed_out;
    let (highlight_from, highlight_to) = if apply {
        move_highlight_squares(mv)
    } else {
        (None, None)
    };
    if apply {
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }

    let mut lines = Vec::new();
    let turn = if pos.side_to_move() == Color::Black {
        "先手番"
    } else {
        "後手番"
    };
    let mut header = vec![Span::raw(format!("手番: {turn}"))];
    if pos.in_check() {
        header.push(Span::raw("  "));
        header.push(Span::styled(
            "王手",
            Style::default().fg(RColor::Red).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));
    lines.push(Line::from(format!("後手持駒: {}", hand_text(&pos, Color::White))));
    lines.push(Line::from(""));

    lines.push(Line::from(file_label_line()));
    lines.push(Line::from(horizontal_border('┌', '┬', '┐')));
    for rank in 0..9u8 {
        let mut spans = vec![Span::raw("│")];
        for file in (0..9u8).rev() {
            let sq_idx = file * 9 + rank;
            let Some(sq) = Square::from_u8(sq_idx) else {
                continue;
            };
            let piece = pos.piece_on(sq);
            let mut style = if piece.is_none() {
                Style::default()
            } else if piece.color() == Color::Black {
                Style::default().fg(RColor::Yellow)
            } else {
                Style::default().fg(RColor::Cyan)
            };
            // 最終手の移動先（駒あり）・移動元（空マス）の両方を背景色＋太字でハイライト
            // する。前景（駒の先後色）を残すので駒は常に読め、太字で視認性を上げる。
            if highlight_to == Some(sq) || highlight_from == Some(sq) {
                style = style.bg(LAST_MOVE_BG).add_modifier(Modifier::BOLD);
            }
            let text = if piece.is_none() {
                empty_cell()
            } else {
                center_cell(board_glyph(piece.piece_type()))
            };
            spans.push(Span::styled(text, style));
            spans.push(Span::raw("│"));
        }
        spans.push(Span::raw(format!(" {}", RANK_LABELS[rank as usize])));
        lines.push(Line::from(spans));
        if rank < 8 {
            lines.push(Line::from(horizontal_border('├', '┼', '┤')));
        }
    }
    lines.push(Line::from(horizontal_border('└', '┴', '┘')));

    lines.push(Line::from(""));
    lines.push(Line::from(format!("先手持駒: {}", hand_text(&pos, Color::Black))));
    lines
}

fn hand_text(pos: &Position, color: Color) -> String {
    const ORDER: [PieceType; 7] = [
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ];
    let hand = pos.hand(color);
    let parts: Vec<String> = ORDER
        .iter()
        .filter_map(|&pt| {
            let n = hand.count(pt);
            if n == 0 {
                None
            } else if n > 1 {
                Some(format!("{}{}", piece_label(pt, false), n))
            } else {
                Some(piece_label(pt, false).to_string())
            }
        })
        .collect();
    if parts.is_empty() {
        "なし".to_string()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{EvalMetrics, MoveAnnotation};
    use super::*;

    #[test]
    fn format_elapsed_prefers_whole_seconds() {
        // CSA の T 行由来 (秒粒度) は整数秒、JSONL 由来の ms 精度は小数 1 桁。
        assert_eq!(format_elapsed(6000), "6s");
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(1234), "1.2s");
        // 0.05 秒境界は整数演算の四捨五入で決定的に丸める。
        assert_eq!(format_elapsed(949), "0.9s");
        assert_eq!(format_elapsed(950), "1.0s");
        assert_eq!(format_elapsed(1050), "1.1s");
        assert_eq!(format_elapsed(1949), "1.9s");
    }

    fn mv(side: Color, score_cp: Option<i32>, score_mate: Option<i32>) -> MoveView {
        mv_with_ply(1, side, score_cp, score_mate)
    }

    fn mv_with_ply(
        ply: u32,
        side: Color,
        score_cp: Option<i32>,
        score_mate: Option<i32>,
    ) -> MoveView {
        MoveView {
            ply,
            side,
            sfen_before: String::new(),
            mv: Move::NONE,
            kif_label: format!("手{ply}"),
            annotation: MoveAnnotation {
                score_cp,
                score_mate,
                ..Default::default()
            },
        }
    }

    fn jsonl_entry(
        game_id: u32,
        pair_index: Option<u32>,
        pair_slot: Option<u32>,
        startpos_idx: Option<u32>,
        outcome: Option<GameOutcomeView>,
        error: bool,
        file_idx: usize,
    ) -> GameIndexEntry {
        GameIndexEntry {
            source: GameSourceRef::Jsonl {
                file_idx,
                game_id,
                start_offset: 0,
                end_offset: 0,
            },
            outcome,
            error,
            ply_count: 1,
            pair_index,
            pair_slot,
            startpos_idx,
            metrics: Default::default(),
        }
    }

    fn empty_index() -> GameIndex {
        GameIndex::default()
    }

    /// `date_key` 付きの pair_files を 1 件持つ索引と、それを指す JSONL エントリを作る。
    fn index_with_dated_file(date_key: Option<u64>) -> (GameIndex, GameIndexEntry) {
        let index = GameIndex {
            entries: Vec::new(),
            pair_files: vec![PairFileMeta {
                path: PathBuf::from("20260707_010203_A_vs_B.jsonl"),
                black_label: "A".to_string(),
                white_label: "B".to_string(),
                date_key,
            }],
            warnings: Vec::new(),
        };
        let entry = jsonl_entry(1, None, None, None, None, false, 0);
        (index, entry)
    }

    #[test]
    fn parse_filter_recognizes_rate_and_date() {
        assert_eq!(parse_filter("rate:>3800"), Filter::Field(FieldKind::Rate, ">3800"));
        assert_eq!(parse_filter("date:20260707"), Filter::Field(FieldKind::Date, "20260707"));
    }

    #[test]
    fn date_filter_prefix_matches_date_key() {
        let (index, entry) = index_with_dated_file(Some(20260707010203));
        assert!(entry_matches(&index, &entry, parse_filter("date:20260707")));
        assert!(entry_matches(&index, &entry, parse_filter("date:202607")));
        assert!(!entry_matches(&index, &entry, parse_filter("date:20260708")));
        // 日時キーの無い対局は date: にヒットしない
        let (index, entry) = index_with_dated_file(None);
        assert!(!entry_matches(&index, &entry, parse_filter("date:2026")));
        // rate: は entry_matches では絞らない(App::apply_filter がレート表で絞る)
        assert!(entry_matches(&index, &entry, parse_filter("rate:>9999")));
    }

    #[test]
    fn sort_date_puts_newest_first_and_none_last() {
        let entries: Vec<GameIndexEntry> = (0..3u32)
            .map(|i| jsonl_entry(i, None, None, None, None, false, i as usize))
            .collect();
        let keys = [Some(20260706010203u64), None, Some(20260707010203u64)];
        let mut filtered = vec![0, 1, 2];
        sort_filtered(&mut filtered, &entries, SortMode::Date, |e| {
            let GameSourceRef::Jsonl { file_idx, .. } = e.source else {
                return None;
            };
            keys[file_idx]
        });
        assert_eq!(filtered, vec![2, 0, 1]); // 新→旧→日時なし
    }

    /// live 再読込テスト用の CSA 形式 fake。共有 `games` を差し替えて対局の成長・追加を
    /// 模せる(実ファイル不要)。
    struct SharedCsaSource(std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>);

    impl GameSource for SharedCsaSource {
        fn build_index(&self) -> Result<GameIndex> {
            let games = self.0.borrow();
            let entries = (0..games.len())
                .map(|i| GameIndexEntry {
                    source: GameSourceRef::Csa {
                        file_idx: i,
                        ordinal: i as u32,
                    },
                    outcome: None,
                    error: false,
                    ply_count: games[i].len() as u32,
                    pair_index: None,
                    pair_slot: None,
                    startpos_idx: None,
                    metrics: Default::default(),
                })
                .collect();
            let pair_files = (0..games.len())
                .map(|i| PairFileMeta {
                    path: PathBuf::from(format!("g{i}.csa")),
                    black_label: "A".to_string(),
                    white_label: "B".to_string(),
                    date_key: None,
                })
                .collect();
            Ok(GameIndex {
                entries,
                pair_files,
                warnings: Vec::new(),
            })
        }

        fn load_game(&self, _index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord> {
            let GameSourceRef::Csa { file_idx, .. } = entry.source else {
                unreachable!("SharedCsaSource yields only Csa refs");
            };
            let moves = self.0.borrow()[file_idx].iter().map(|s| move_with_sfen(s)).collect();
            Ok(GameRecord {
                moves,
                leading_gap_is_drop: false,
                termination: None,
            })
        }
    }

    #[test]
    fn replace_index_reloads_grown_game_and_follows_tail() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let games = Rc::new(RefCell::new(vec![
            vec![HIRATE_SFEN.to_string(); 2],
            vec![HIRATE_SFEN.to_string(); 3],
        ]));
        let source = SharedCsaSource(games.clone());
        let index = source.build_index().unwrap();
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        // 対局 1 (3 手) を選び末尾の手を表示
        app.next_game();
        app.current_move = 2;
        assert_eq!(app.current_game.as_ref().unwrap().moves.len(), 3);

        // 対局 1 が 5 手へ伸び、新しい対局が 1 局増えた
        games.borrow_mut()[1] = vec![HIRATE_SFEN.to_string(); 5];
        games.borrow_mut().push(vec![HIRATE_SFEN.to_string(); 1]);
        let new_index = app.source.build_index().unwrap();
        app.replace_index(new_index);
        // 選択は同じ対局のまま読み直され、末尾を見ていたので新しい末尾へ追従
        assert_eq!(app.selected, 1);
        assert_eq!(app.current_game.as_ref().unwrap().moves.len(), 5);
        assert_eq!(app.current_move, 4);
        assert!(app.status.contains("1 局追加"), "status: {}", app.status);

        // 変化の無い完了局は読み直さず手の位置を維持する
        app.prev_game();
        app.current_move = 1;
        let new_index = app.source.build_index().unwrap();
        app.replace_index(new_index);
        assert_eq!(app.selected, 0);
        assert_eq!(app.current_move, 1);
    }

    #[test]
    fn home_end_jump_to_first_and_last_move() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let games = Rc::new(RefCell::new(vec![vec![HIRATE_SFEN.to_string(); 4]]));
        let source = SharedCsaSource(games);
        let index = source.build_index().unwrap();
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        app.last_move();
        assert_eq!(app.current_move, 3);
        app.first_move();
        assert_eq!(app.current_move, 0);
    }

    #[test]
    fn follow_jumps_to_tail_on_game_switch_and_breaks_on_back_navigation() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let games = Rc::new(RefCell::new(vec![
            vec![HIRATE_SFEN.to_string(); 2],
            vec![HIRATE_SFEN.to_string(); 4],
        ]));
        let source = SharedCsaSource(games);
        let index = source.build_index().unwrap();
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        app.follow = true;
        // 対局切替で開いた対局の最新手から表示する
        app.next_game();
        assert_eq!(app.current_move, 3);
        // 位置が動かない空振り操作（末尾で急変ジャンプ、評価値なし）では解除しない
        app.nav_breaking_follow(App::jump_to_next_eval_swing);
        assert!(app.follow);
        assert_eq!(app.current_move, 3);
        // 手を戻す操作で追従は自動解除される
        app.nav_breaking_follow(App::prev_move);
        assert!(!app.follow);
        assert_eq!(app.current_move, 2);
    }

    #[test]
    fn replace_index_with_follow_jumps_to_tail_from_middle() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let games = Rc::new(RefCell::new(vec![vec![HIRATE_SFEN.to_string(); 3]]));
        let source = SharedCsaSource(games.clone());
        let index = source.build_index().unwrap();
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        // 途中の手を表示していても、追従中は伸びた対局の新しい末尾へジャンプする
        app.follow = true;
        app.current_move = 0;
        games.borrow_mut()[0] = vec![HIRATE_SFEN.to_string(); 6];
        let new_index = app.source.build_index().unwrap();
        app.replace_index(new_index);
        assert_eq!(app.current_move, 5);
    }

    #[test]
    fn combined_rate_takes_max_and_tolerates_missing_side() {
        assert_eq!(combined_rate(Some(3700.0), Some(3500.0)), Some(3700.0));
        assert_eq!(combined_rate(None, Some(3500.0)), Some(3500.0));
        assert_eq!(combined_rate(Some(3700.0), None), Some(3700.0));
        assert_eq!(combined_rate(None, None), None);
    }

    #[test]
    fn signed_cmp_supports_negative_ratings() {
        // floodgate には負レートのプレイヤーが実在する。0 に clamp せず符号付きで比較する。
        assert!(matches_signed_cmp(-1769, "-1769"));
        assert!(!matches_signed_cmp(-1769, "0"));
        assert!(matches_signed_cmp(-1769, "<0"));
        assert!(matches_signed_cmp(-100, ">-500"));
        assert!(!matches_signed_cmp(-1769, ">-500"));
        assert!(matches_signed_cmp(3706, ">3500"));
        assert!(!matches_signed_cmp(3706, ">3800"));
    }

    #[test]
    fn eval_points_preserves_gap_position_for_missing_eval() {
        // 中央の手だけ評価値が無い対局。eval_points は評価値付きの手の元インデックスを
        // 保つため None を潰さず、位置が元の手の並びと一致することを固定する
        // （`evaluated_points` / 評価値急変ジャンプがこの index を使う）。
        let game = GameRecord {
            moves: vec![
                mv(Color::Black, Some(10), None),
                mv(Color::White, None, None),
                mv(Color::Black, Some(-5), None),
            ],
            leading_gap_is_drop: false,
            termination: None,
        };
        let points = eval_points(&game);
        assert_eq!(points.len(), 3);
        assert!(points[0].is_some());
        assert!(points[1].is_none(), "評価値の無い手は None のまま保持される");
        assert!(points[2].is_some());
    }

    // --- 検索フィルタ (parse_filter / entry_matches) ---

    #[test]
    fn parse_filter_recognizes_known_field_prefixes() {
        assert_eq!(parse_filter("pair:4"), Filter::Field(FieldKind::Pair, "4"));
        assert_eq!(parse_filter("slot:1"), Filter::Field(FieldKind::Slot, "1"));
        assert_eq!(parse_filter("startpos:2"), Filter::Field(FieldKind::Startpos, "2"));
        assert_eq!(parse_filter("id:11"), Filter::Field(FieldKind::Id, "11"));
        assert_eq!(parse_filter("outcome:draw"), Filter::Field(FieldKind::Outcome, "draw"));
        assert_eq!(parse_filter("label:vol4b"), Filter::Field(FieldKind::Label, "vol4b"));
    }

    #[test]
    fn parse_filter_unknown_prefix_falls_back_to_text() {
        // ":" を含むが既知の field 名ではない場合はテキスト検索として扱う
        // （コロンを含む対局ラベル等を将来 label に持つ可能性を潰さないため）。
        assert_eq!(parse_filter("foo:bar"), Filter::Text("foo:bar"));
    }

    #[test]
    fn parse_filter_numeric_only_disables_label_substring() {
        assert_eq!(parse_filter("4"), Filter::NumericExact("4"));
    }

    #[test]
    fn parse_filter_text_fallback_for_non_numeric_query() {
        assert_eq!(parse_filter("vol4b_raw"), Filter::Text("vol4b_raw"));
    }

    // --- SFEN 局面検索 ---

    const HIRATE_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn move_with_sfen(sfen: &str) -> MoveView {
        MoveView {
            ply: 1,
            side: Color::Black,
            sfen_before: sfen.to_string(),
            mv: Move::NONE,
            kif_label: String::new(),
            annotation: MoveAnnotation::default(),
        }
    }

    #[test]
    fn normalize_sfen_strips_move_number_and_preserves_case() {
        let base = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
        // 手数だけ違う同一局面は正規化後に一致する。
        assert_eq!(normalize_sfen(&format!("{base} 1")), normalize_sfen(&format!("{base} 77")));
        // 手数フィールドを落とし、大文字（先手駒）を保持する。
        assert_eq!(normalize_sfen(&format!("{base} 1")), base);
        assert!(normalize_sfen(&format!("{base} 1")).contains("PPPPPPPPP"));
    }

    #[test]
    fn game_contains_sfen_matches_ignoring_move_number() {
        let game = GameRecord {
            moves: vec![
                move_with_sfen("9/9/9/9/9/9/9/9/9 b - 5"),
                move_with_sfen(HIRATE_SFEN),
            ],
            leading_gap_is_drop: false,
            termination: None,
        };
        // 手数が違っても盤面・手番・持駒が一致すればヒットする。
        assert!(game_contains_sfen(&game, &normalize_sfen("9/9/9/9/9/9/9/9/9 b - 1")));
        // 手番が違えばヒットしない。
        assert!(!game_contains_sfen(&game, &normalize_sfen("9/9/9/9/9/9/9/9/9 w - 1")));
    }

    #[test]
    fn parse_filter_recognizes_sfen_prefix() {
        assert_eq!(parse_filter("sfen:9/9/9 b - 1"), Filter::Sfen("9/9/9 b - 1"));
    }

    #[test]
    fn entry_matches_sfen_is_always_true_for_immediate_filter() {
        // 即時フィルタでは絞らない（実際の判定は Enter 契機の逐次スキャン）。
        let index = empty_index();
        let entry = jsonl_entry(1, None, None, None, None, false, 0);
        assert!(entry_matches(&index, &entry, Filter::Sfen("anything")));
    }

    struct FakeSource {
        /// 各対局の着手前 SFEN 列。
        games: Vec<Vec<String>>,
        /// `load_game` が `Err` を返す対局 ordinal（照合対象から除外する分岐の検証用）。
        error_ordinals: Vec<usize>,
    }

    fn fake_source(games: Vec<Vec<String>>) -> FakeSource {
        FakeSource {
            games,
            error_ordinals: Vec::new(),
        }
    }

    impl GameSource for FakeSource {
        fn build_index(&self) -> Result<GameIndex> {
            let entries = (0..self.games.len())
                .map(|i| GameIndexEntry {
                    source: GameSourceRef::Psv {
                        start_record: 0,
                        end_record: 0,
                        ordinal: i as u32,
                    },
                    outcome: None,
                    error: false,
                    ply_count: self.games[i].len() as u32,
                    pair_index: None,
                    pair_slot: None,
                    startpos_idx: None,
                    metrics: Default::default(),
                })
                .collect();
            Ok(GameIndex {
                entries,
                pair_files: Vec::new(),
                warnings: Vec::new(),
            })
        }

        fn load_game(&self, _index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord> {
            let GameSourceRef::Psv { ordinal, .. } = entry.source else {
                unreachable!("FakeSource yields only Psv refs");
            };
            if self.error_ordinals.contains(&(ordinal as usize)) {
                anyhow::bail!("synthetic load error for game {ordinal}");
            }
            let moves = self.games[ordinal as usize].iter().map(|s| move_with_sfen(s)).collect();
            Ok(GameRecord {
                moves,
                leading_gap_is_drop: false,
                termination: None,
            })
        }
    }

    /// スキャンが有限回で完了するまで進める。
    fn drain_scan(app: &mut App) {
        let mut guard = 0;
        while app.scan.is_some() {
            app.advance_sfen_scan();
            guard += 1;
            assert!(guard < 1000, "スキャンが有限回で終わる");
        }
    }

    #[test]
    fn parse_sfen_query_validates_field_count() {
        assert_eq!(parse_sfen_query("9/9/9 b -"), Some("9/9/9 b -".to_string()));
        assert_eq!(parse_sfen_query("9/9/9 b - 1"), Some("9/9/9 b -".to_string()));
        // 手数だけ違っても同じ比較キーになる。
        assert_eq!(parse_sfen_query("9/9/9 b - 1"), parse_sfen_query("9/9/9 b - 42"));
        // フィールド不足・余剰・手数が非数値・空は None（走査しない）。
        assert_eq!(parse_sfen_query("9/9/9 b"), None);
        assert_eq!(parse_sfen_query("9/9/9"), None);
        assert_eq!(parse_sfen_query("9/9/9 b - 1 garbage"), None);
        assert_eq!(parse_sfen_query("9/9/9 b - x"), None);
        assert_eq!(parse_sfen_query(""), None);
    }

    #[test]
    fn sfen_query_prefix_is_case_insensitive_but_value_is_preserved() {
        assert_eq!(sfen_query_from_input("sfen:9/9/9 b - 1"), Some("9/9/9 b - 1"));
        // プレフィクスは大小無視、値（大文字＝先手駒）は原文のまま。
        assert_eq!(sfen_query_from_input("SFEN:LNSG b - 1"), Some("LNSG b - 1"));
        assert_eq!(sfen_query_from_input("Sfen:x"), Some("x"));
        assert_eq!(sfen_query_from_input("pair:4"), None);
        assert_eq!(sfen_query_from_input("sfe"), None);
        // マルチバイト先頭でも panic しない。
        assert_eq!(sfen_query_from_input("日本語"), None);
    }

    #[test]
    fn sfen_scan_filters_to_games_containing_position() {
        // game 0/2 は探索局面を含む（2 は手数だけ違う）。game 1 は含まない。
        let source = fake_source(vec![
            vec![
                "9/9/9/9/9/9/9/9/9 b - 1".to_string(),
                HIRATE_SFEN.to_string(),
            ],
            vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()],
            vec!["lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 40".to_string()],
        ]);
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);

        app.start_sfen_scan(HIRATE_SFEN);
        drain_scan(&mut app);
        assert_eq!(app.filtered, vec![0, 2], "探索局面を含む対局だけに絞られる");
        assert!(app.status.contains("2 件"), "一致件数を表示: {}", app.status);
    }

    #[test]
    fn sfen_scan_spans_multiple_chunks() {
        // SFEN_SCAN_CHUNK を超える対局数で、tick 跨ぎの take/書き戻しと一致蓄積を検証する。
        let n = SFEN_SCAN_CHUNK * 2 + 5;
        let mut games = vec![vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()]; n];
        games[5] = vec![HIRATE_SFEN.to_string()];
        games[SFEN_SCAN_CHUNK + 3] =
            vec!["lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 7".to_string()];
        let source = fake_source(games);
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);

        app.start_sfen_scan(HIRATE_SFEN);
        let mut ticks = 0;
        while app.scan.is_some() {
            app.advance_sfen_scan();
            ticks += 1;
            assert!(ticks < 1000, "有限回で終わる");
        }
        assert!(ticks >= 2, "複数 tick に跨る (実測 {ticks})");
        assert_eq!(
            app.filtered,
            vec![5, SFEN_SCAN_CHUNK + 3],
            "chunk 跨ぎでも一致を取りこぼさない"
        );
    }

    #[test]
    fn cancel_sfen_scan_keeps_prior_filter() {
        // 別フィルタで部分集合に絞った状態から SFEN スキャンを途中中断し、filtered と
        // base_filtered がどちらもスキャン前（＝別フィルタの結果）のままであることを確認する。
        let n = SFEN_SCAN_CHUNK * 2;
        let mut games = vec![vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()]; n];
        for i in [1usize, 40, 63] {
            games[i] = vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string(); 3]; // len:3 で絞れる部分集合。
        }
        let source = fake_source(games);
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        app.filter_input = "len:3".to_string();
        app.apply_filter();
        let filtered_before = app.filtered.clone();
        let base_before = app.base_filtered.clone();
        assert_eq!(base_before, vec![1, 40, 63], "事前フィルタで部分集合になっている");

        app.start_sfen_scan(HIRATE_SFEN);
        app.advance_sfen_scan(); // 1 チャンクだけ進め、未完了のまま中断する。
        assert!(app.scan.is_some(), "まだ走査途中");
        app.cancel_sfen_scan();
        assert!(app.scan.is_none());
        assert_eq!(app.filtered, filtered_before, "中断で filtered はスキャン前のまま");
        assert_eq!(app.base_filtered, base_before, "中断で base_filtered もスキャン前のまま");
    }

    #[test]
    fn sfen_scan_skips_games_that_fail_to_load() {
        // game 1 は探索局面を含むが load_game が Err → 照合対象から除外。game 2 は含み load 成功。
        let source = FakeSource {
            games: vec![
                vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()],
                vec![HIRATE_SFEN.to_string()],
                vec![HIRATE_SFEN.to_string()],
            ],
            error_ordinals: vec![1],
        };
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        app.start_sfen_scan(HIRATE_SFEN);
        drain_scan(&mut app);
        assert_eq!(app.filtered, vec![2], "load 失敗の対局は一致に含めない");
    }

    #[test]
    fn sort_after_sfen_scan_preserves_matches() {
        // スキャン後に `s`（並べ替え）を押しても絞り込み結果が全件に戻らないこと（回帰）。
        let source = fake_source(vec![
            vec![
                "9/9/9/9/9/9/9/9/9 b - 1".to_string(),
                HIRATE_SFEN.to_string(),
            ],
            vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()],
            vec!["lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 40".to_string()],
        ]);
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        // Enter 経由と同じく filter_input を残したままスキャン（`s` の再フィルタ源）。
        app.filter_input = format!("sfen:{HIRATE_SFEN}");
        app.start_sfen_scan(HIRATE_SFEN);
        drain_scan(&mut app);
        assert_eq!(app.filtered, vec![0, 2]);

        app.cycle_sort_mode();
        let mut got = app.filtered.clone();
        got.sort_unstable();
        assert_eq!(got, vec![0, 2], "並べ替えでスキャン結果が維持される（全件に戻らない）");
    }

    #[test]
    fn invalid_sfen_query_does_not_start_scan() {
        let source = fake_source(vec![vec!["9/9/9/9/9/9/9/9/9 b - 1".to_string()]]);
        let index = source.build_index().expect("build_index");
        let mut app = App::new(Box::new(source), index, BTreeMap::new(), None);
        app.start_sfen_scan("   "); // 空
        assert!(app.scan.is_none(), "空クエリではスキャンを開始しない");
        assert!(app.status.contains("不正"), "不正入力はエラー表示: {}", app.status);
        app.start_sfen_scan("9/9/9 b"); // フィールド不足
        assert!(app.scan.is_none(), "フィールド不足ではスキャンを開始しない");
    }

    #[test]
    fn numeric_query_does_not_match_label_substring_but_matches_exact_pair_index() {
        // "vol4B_raw" のようにラベルに数字を含むデータでは、pair_index=4 の
        // 絞り込みのつもりで "4" と打ってもラベル部分一致でノイズが出てしまう。
        let index = empty_index();
        let entry = jsonl_entry(1, Some(4), None, None, None, false, 0);
        let filter = parse_filter("4");
        assert!(entry_matches(&index, &entry, filter), "pair_index=4 は数値完全一致でヒットする");

        // ラベルにしか "4" を含まない対局は数値クエリではヒットしない。
        let entry_label_only = jsonl_entry(1, Some(9), None, None, None, false, 0);
        assert!(!entry_matches(&index, &entry_label_only, filter));
    }

    #[test]
    fn field_prefix_matches_only_the_specified_field() {
        let index = empty_index();
        let entry = jsonl_entry(1, Some(4), Some(0), Some(2), None, false, 0);
        assert!(entry_matches(&index, &entry, Filter::Field(FieldKind::Pair, "4")));
        assert!(!entry_matches(&index, &entry, Filter::Field(FieldKind::Pair, "5")));
        assert!(entry_matches(&index, &entry, Filter::Field(FieldKind::Slot, "0")));
        assert!(entry_matches(&index, &entry, Filter::Field(FieldKind::Startpos, "2")));
        assert!(entry_matches(&index, &entry, Filter::Field(FieldKind::Id, "1")));
    }

    #[test]
    fn field_outcome_matches_error_and_win_keywords() {
        let index = empty_index();
        let error_entry = jsonl_entry(1, None, None, None, None, true, 0);
        assert!(entry_matches(&index, &error_entry, Filter::Field(FieldKind::Outcome, "error")));

        let win_entry =
            jsonl_entry(2, None, None, None, Some(GameOutcomeView::Win(Color::Black)), false, 0);
        assert!(entry_matches(
            &index,
            &win_entry,
            Filter::Field(FieldKind::Outcome, "black_win")
        ));
    }

    #[test]
    fn empty_filter_matches_everything() {
        let index = empty_index();
        let entry = jsonl_entry(1, None, None, None, None, false, 0);
        assert!(entry_matches(&index, &entry, Filter::Empty));
    }

    // --- ソート/グループ化 ---

    #[test]
    fn sort_mode_cycles_through_all_variants() {
        assert_eq!(SortMode::Discovery.next(), SortMode::Date);
        assert_eq!(SortMode::Date.next(), SortMode::Outcome);
        assert_eq!(SortMode::Outcome.next(), SortMode::Length);
        assert_eq!(SortMode::Length.next(), SortMode::Decisiveness);
        assert_eq!(SortMode::Decisiveness.next(), SortMode::Swing);
        assert_eq!(SortMode::Swing.next(), SortMode::Discovery);
    }

    fn metric_entry(
        ply_count: u32,
        final_cp: Option<i32>,
        max_swing_cp: Option<u32>,
    ) -> GameIndexEntry {
        GameIndexEntry {
            source: GameSourceRef::Psv {
                start_record: 0,
                end_record: 0,
                ordinal: 0,
            },
            outcome: None,
            error: false,
            ply_count,
            pair_index: None,
            pair_slot: None,
            startpos_idx: None,
            metrics: EvalMetrics {
                final_cp,
                min_cp: final_cp,
                max_cp: final_cp,
                max_swing_cp,
            },
        }
    }

    #[test]
    fn sort_filtered_by_outcome_groups_errors_first_and_keeps_discovery_order_within_group() {
        let entries = vec![
            jsonl_entry(1, None, None, None, Some(GameOutcomeView::Draw), false, 0), // idx 0: draw
            jsonl_entry(2, None, None, None, Some(GameOutcomeView::Win(Color::Black)), false, 0), // idx 1: b-win
            jsonl_entry(3, None, None, None, None, true, 0), // idx 2: error
            jsonl_entry(4, None, None, None, Some(GameOutcomeView::Win(Color::Black)), false, 0), // idx 3: b-win
        ];
        let mut filtered: Vec<usize> = (0..entries.len()).collect();
        sort_filtered(&mut filtered, &entries, SortMode::Outcome, |_| None);
        // error(2) → b-win(1,3、発見順維持) → draw(0)
        assert_eq!(filtered, vec![2, 1, 3, 0]);
    }

    #[test]
    fn sort_filtered_metric_sorts_are_descending_with_none_last() {
        let entries = vec![
            metric_entry(30, Some(50), Some(100)),   // idx 0
            metric_entry(200, Some(-800), Some(50)), // idx 1
            metric_entry(100, None, None),           // idx 2: 指標なし
            metric_entry(80, Some(300), Some(900)),  // idx 3
        ];
        let key = |mode| {
            let mut f: Vec<usize> = (0..entries.len()).collect();
            sort_filtered(&mut f, &entries, mode, |_| None);
            f
        };
        // 対局長 降順。
        assert_eq!(key(SortMode::Length), vec![1, 2, 3, 0]);
        // |final_cp| 降順、None(idx2) 末尾。|−800| > |300| > |50|。
        assert_eq!(key(SortMode::Decisiveness), vec![1, 3, 0, 2]);
        // max_swing_cp 降順、None(idx2) 末尾。900 > 100 > 50。
        assert_eq!(key(SortMode::Swing), vec![3, 0, 1, 2]);
    }

    #[test]
    fn entry_matches_tier2_predicates() {
        let idx = GameIndex::default();
        let b_win =
            jsonl_entry(1, None, None, None, Some(GameOutcomeView::Win(Color::Black)), false, 0);
        let w_win =
            jsonl_entry(2, None, None, None, Some(GameOutcomeView::Win(Color::White)), false, 0);
        let draw = jsonl_entry(3, None, None, None, Some(GameOutcomeView::Draw), false, 0);
        assert!(entry_matches(&idx, &b_win, parse_filter("winner:sente")));
        assert!(!entry_matches(&idx, &b_win, parse_filter("winner:gote")));
        assert!(entry_matches(&idx, &w_win, parse_filter("winner:gote")));
        // black/white は sente/gote の別名。
        assert!(entry_matches(&idx, &b_win, parse_filter("winner:black")));
        assert!(entry_matches(&idx, &w_win, parse_filter("winner:white")));
        assert!(entry_matches(&idx, &b_win, parse_filter("decisive")));
        assert!(!entry_matches(&idx, &draw, parse_filter("decisive")));

        let long = metric_entry(150, None, None);
        assert!(entry_matches(&idx, &long, parse_filter("len:>100")));
        assert!(!entry_matches(&idx, &long, parse_filter("len:<100")));
        assert!(entry_matches(&idx, &metric_entry(30, None, None), parse_filter("len:30")));

        let swingy = metric_entry(10, Some(0), Some(500));
        assert!(entry_matches(&idx, &swingy, parse_filter("swing:>300")));
        assert!(!entry_matches(&idx, &swingy, parse_filter("swing:>600")));

        let mut rev = metric_entry(10, Some(100), Some(50));
        rev.metrics.min_cp = Some(-400);
        rev.metrics.max_cp = Some(400);
        assert!(entry_matches(&idx, &rev, parse_filter("reversal")));
        // min=max=100（両者優勢局面なし）は逆転ではない。
        assert!(!entry_matches(
            &idx,
            &metric_entry(10, Some(100), Some(50)),
            parse_filter("reversal")
        ));
    }

    #[test]
    fn sort_filtered_discovery_mode_is_identity() {
        let entries = vec![
            jsonl_entry(1, None, None, None, None, true, 0),
            jsonl_entry(2, None, None, None, None, false, 0),
        ];
        let mut filtered: Vec<usize> = (0..entries.len()).collect();
        sort_filtered(&mut filtered, &entries, SortMode::Discovery, |_| None);
        assert_eq!(filtered, vec![0, 1]);
    }

    // --- |Δcp| 閾値ジャンプ ---

    fn game_with_evals(evals: &[Option<i32>]) -> GameRecord {
        let moves = evals
            .iter()
            .enumerate()
            .map(|(i, cp)| mv_with_ply((i + 1) as u32, Color::Black, *cp, None))
            .collect();
        GameRecord {
            moves,
            leading_gap_is_drop: false,
            termination: None,
        }
    }

    #[test]
    fn next_eval_swing_finds_first_large_jump_after_current_move() {
        // 0 -> 10 (小変動) -> 300 (急騰、閾値超え) -> 320 (小変動)
        let game = game_with_evals(&[Some(0), Some(10), Some(300), Some(320)]);
        assert_eq!(next_eval_swing(&game, 0, EVAL_SWING_THRESHOLD_CP), Some(2));
        // 急変後から探すと、その先には無い。
        assert_eq!(next_eval_swing(&game, 2, EVAL_SWING_THRESHOLD_CP), None);
    }

    #[test]
    fn next_eval_swing_skips_moves_without_eval() {
        // eval が無い手 (index 1) を挟んでも、評価値付きの手同士で Δ を見る。
        let game = game_with_evals(&[Some(0), None, Some(300)]);
        assert_eq!(next_eval_swing(&game, 0, EVAL_SWING_THRESHOLD_CP), Some(2));
    }

    #[test]
    fn prev_eval_swing_finds_nearest_large_jump_before_current_move() {
        let game = game_with_evals(&[Some(0), Some(300), Some(310), Some(320)]);
        assert_eq!(prev_eval_swing(&game, 3, EVAL_SWING_THRESHOLD_CP), Some(1));
        assert_eq!(prev_eval_swing(&game, 1, EVAL_SWING_THRESHOLD_CP), None);
    }

    // --- エラー対局(0手)の表示 ---

    #[test]
    fn empty_state_reports_error_game_distinctly_from_plain_empty_game() {
        let error_entry = jsonl_entry(1, None, None, None, None, true, 0);
        let empty_game = GameRecord {
            moves: Vec::new(),
            leading_gap_is_drop: false,
            termination: None,
        };
        assert_eq!(
            empty_state(Some(&error_entry), "", Some(&empty_game)),
            Some(EmptyState::ErrorGame)
        );

        let non_error_entry =
            jsonl_entry(2, None, None, None, Some(GameOutcomeView::Draw), false, 0);
        assert_eq!(
            empty_state(Some(&non_error_entry), "", Some(&empty_game)),
            Some(EmptyState::EmptyGame)
        );
    }

    #[test]
    fn empty_state_none_when_game_has_moves() {
        let entry = jsonl_entry(1, None, None, None, None, false, 0);
        let game = GameRecord {
            moves: vec![mv(Color::Black, Some(0), None)],
            leading_gap_is_drop: false,
            termination: None,
        };
        assert_eq!(empty_state(Some(&entry), "", Some(&game)), None);
    }

    #[test]
    fn empty_state_reports_no_selection_and_load_failure() {
        assert_eq!(empty_state(None, "", None), Some(EmptyState::NoSelection));
        let entry = jsonl_entry(1, None, None, None, None, false, 0);
        assert_eq!(empty_state(Some(&entry), "読み込み失敗", None), Some(EmptyState::LoadFailed));
    }

    // --- PSV の手数欠番 ---

    #[test]
    fn ply_gap_before_detects_skipped_plies() {
        let game = GameRecord {
            moves: vec![
                mv_with_ply(1, Color::Black, None, None),
                mv_with_ply(4, Color::White, None, None),
            ],
            leading_gap_is_drop: true,
            termination: None,
        };
        assert_eq!(ply_gap_before(&game, 0), None, "先頭が ply=1 なら先頭欠番はない");
        assert_eq!(ply_gap_before(&game, 1), Some(2), "1 の次が 4 なら 2,3 の 2 手が欠落");
    }

    #[test]
    fn ply_gap_before_detects_leading_gap_from_skip_initial_ply() {
        // skip_initial_ply により最初の記録レコードが ply=1 より後ろから
        // 始まるケース（対局内の隣接手同士では検出できない先頭欠番）。
        let game = GameRecord {
            moves: vec![
                mv_with_ply(12, Color::Black, None, None),
                mv_with_ply(13, Color::White, None, None),
            ],
            leading_gap_is_drop: true,
            termination: None,
        };
        assert_eq!(ply_gap_before(&game, 0), Some(11), "ply=12 開始なら 1〜11 の 11 手が欠落");
        assert_eq!(ply_gap_before(&game, 1), None, "12 の次が 13 なら欠番なし");
    }

    #[test]
    fn ply_gap_before_no_leading_marker_for_jsonl_book_start() {
        // JSONL の定跡途中開始（先頭 ply=24）は欠落ではないので先頭マーカーを出さない。
        let game = GameRecord {
            moves: vec![
                mv_with_ply(24, Color::Black, None, None),
                mv_with_ply(25, Color::White, None, None),
            ],
            leading_gap_is_drop: false,
            termination: None,
        };
        assert_eq!(ply_gap_before(&game, 0), None, "定跡開始は先頭欠番扱いにしない");
        assert_eq!(ply_gap_before(&game, 1), None);
    }

    #[test]
    fn annotation_inline_uses_key_value_and_is_empty_without_data() {
        let mut m = mv_with_ply(1, Color::Black, None, None);
        assert_eq!(annotation_inline(&m), "", "注釈が無ければ空文字（行に何も足さない）");
        m.annotation = MoveAnnotation {
            score_cp: Some(-77),
            depth: Some(15),
            seldepth: Some(20),
            ..Default::default()
        };
        let s = annotation_inline(&m);
        assert!(
            s.contains("評価値-77") && s.contains("depth=15") && s.contains("seldepth=20"),
            "評価値と探索情報を key=value で出す: {s}"
        );
    }

    #[test]
    fn annotation_inline_shows_black_pov_score() {
        // 手番相対で格納した score を、パネルでは先手基準へ変換して表示する（グラフと符号統一）。
        // 先手手はそのまま。
        let b = mv_with_ply(1, Color::Black, Some(80), None);
        assert!(annotation_inline(&b).contains("評価値+80"), "{}", annotation_inline(&b));
        // 後手手（後手にとって +80）は先手基準で -80。
        let mut w = mv_with_ply(2, Color::White, Some(80), None);
        assert!(annotation_inline(&w).contains("評価値-80"), "{}", annotation_inline(&w));
        // 詰みも先手基準（後手詰みは -）。
        w.annotation.score_cp = None;
        w.annotation.score_mate = Some(3);
        assert!(annotation_inline(&w).contains("詰み-3"), "{}", annotation_inline(&w));
    }

    #[test]
    fn ply_gap_before_does_not_underflow_when_ply_does_not_increase() {
        // 壊れた/想定外の入力で ply が減る・同値になるケースでも、
        // 条件が false の枝で `cur_ply - prev_ply - 1` を評価して
        // u32 アンダーフローしないことを固定する。
        let game = GameRecord {
            moves: vec![
                mv_with_ply(5, Color::Black, None, None),
                mv_with_ply(5, Color::White, None, None),
                mv_with_ply(3, Color::Black, None, None),
            ],
            leading_gap_is_drop: false,
            termination: None,
        };
        assert_eq!(ply_gap_before(&game, 1), None);
        assert_eq!(ply_gap_before(&game, 2), None);
    }

    // --- 着手ハイライト ---

    #[test]
    fn move_highlight_squares_normal_move_has_both_from_and_to() {
        let mv = Move::new_move(Square::SQ_11, Square::SQ_55, false);
        assert_eq!(move_highlight_squares(mv), (Some(Square::SQ_11), Some(Square::SQ_55)));
    }

    #[test]
    fn move_highlight_squares_drop_has_only_to() {
        let mv = Move::new_drop(PieceType::Pawn, Square::SQ_55);
        assert_eq!(move_highlight_squares(mv), (None, Some(Square::SQ_55)));
    }

    #[test]
    fn move_highlight_squares_none_for_non_normal_move() {
        assert_eq!(move_highlight_squares(Move::NONE), (None, None));
    }

    // --- 盤面セル幅・罫線 ---

    #[test]
    fn board_glyph_is_single_char_for_every_piece() {
        use PieceType::*;
        // 盤面グリフは罫線と揃えるため、成り駒を含め必ず全角一文字。
        for pt in [
            Pawn, Lance, Knight, Silver, Gold, Bishop, Rook, King, ProPawn, ProLance, ProKnight,
            ProSilver, Horse, Dragon,
        ] {
            assert_eq!(board_glyph(pt).chars().count(), 1, "{pt:?}");
        }
        // 成香/成桂/成銀 は盤面では一文字表記（杏/圭/全）になる。
        assert_eq!(board_glyph(ProLance), "杏");
        assert_eq!(board_glyph(ProKnight), "圭");
        assert_eq!(board_glyph(ProSilver), "全");
    }

    #[test]
    fn center_cell_fills_cell_width_and_centers_glyph() {
        // 半角スペース(1カラム)×spaces + 全角グリフ(2カラム) == CELL_WIDTH。
        let c = center_cell("玉");
        let spaces = c.chars().filter(|ch| *ch == ' ').count();
        assert_eq!(spaces + 2, CELL_WIDTH, "セルは CELL_WIDTH カラムに揃う");
        assert!(c.contains('玉'));
        // 偶数幅なら左右対称。
        if CELL_WIDTH.is_multiple_of(2) {
            let left = c.chars().take_while(|ch| *ch == ' ').count();
            let right = c.chars().rev().take_while(|ch| *ch == ' ').count();
            assert_eq!(left, right, "偶数幅は左右対称に中央寄せ");
        }
    }

    #[test]
    fn horizontal_border_has_nine_cells_and_correct_corners() {
        let border = horizontal_border('┌', '┬', '┐');
        assert!(border.starts_with('┌'));
        assert!(border.ends_with('┐'));
        assert_eq!(border.chars().filter(|&c| c == '┬').count(), 8, "9マス間の交点は8箇所");
        assert_eq!(border.chars().filter(|&c| c == '─').count(), CELL_WIDTH * 9);
    }

    // --- 盤面レンダリング（着手適用・指了後局面・成り駒グリフ） ---

    const HIRATE: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn joined(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn render_board_applies_move_and_flips_turn() {
        // ▲７六歩: 7七(idx 60)→7六(idx 59)。index = (筋-1)*9 + (段-1)。
        let mv = Move::new_move(Square::from_u8(60).unwrap(), Square::from_u8(59).unwrap(), false);
        let after = joined(&render_board(HIRATE, mv, false));
        let none = joined(&render_board(HIRATE, Move::NONE, false));
        assert_ne!(after, none, "通常手は do_move で盤面に反映される");
        assert!(!after.contains("先手番") && after.contains("後手番"), "指了後は後手番");
        assert!(none.contains("先手番"), "Move::NONE は do_move せず手番も変わらない");
    }

    #[test]
    fn render_board_does_not_apply_timed_out_move() {
        // タイムアウト行の bestmove は実際には指されていないので適用しない
        // （盤面・手番は指了前のまま）。
        let mv = Move::new_move(Square::from_u8(60).unwrap(), Square::from_u8(59).unwrap(), false);
        let timed_out = joined(&render_board(HIRATE, mv, true));
        let none = joined(&render_board(HIRATE, Move::NONE, false));
        assert_eq!(timed_out, none, "timed_out の手は do_move せず指了前の局面のまま");
        assert!(timed_out.contains("先手番"), "手番も変わらない");
    }

    #[test]
    fn render_board_shows_promoted_pieces_as_single_char_glyph() {
        // 各成り駒を1つずつ置いた局面（Move::NONE なので do_move しない）。
        let sfen = "3k5/9/9/9/+P+L+N+S+B+R3/9/9/9/3K5 b - 1";
        let s = joined(&render_board(sfen, Move::NONE, false));
        for g in ["と", "杏", "圭", "全", "馬", "龍"] {
            assert!(s.contains(g), "成り駒 {g} を一文字グリフで表示");
        }
        assert!(!s.contains('成') && !s.contains('+'), "盤面に 成/+ の生表記は出さない");
    }

    #[test]
    fn render_board_unparsable_sfen_shows_placeholder() {
        let lines = render_board("not-a-sfen", Move::NONE, false);
        assert_eq!(joined(&lines), "(局面を表示できません)");
    }

    #[test]
    fn black_pov_cp_sign_mate_and_missing_eval() {
        for (color, cp, mate, expected) in [
            (Color::Black, Some(120), None, Some(120.0)),
            (Color::White, Some(80), None, Some(-80.0)),
            (Color::White, None, Some(3), Some(-GRAPH_CP_CLAMP)),
            (Color::Black, None, Some(-2), Some(-GRAPH_CP_CLAMP)),
            (Color::Black, None, None, None),
        ] {
            assert_eq!(black_pov_cp(&mv(color, cp, mate)), expected);
        }
    }
}
