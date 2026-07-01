//! ratatui ベースの棋譜プレイヤー画面・イベントループ。

use std::io::{self};

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

use super::model::{
    GameIndexEntry, GameOutcomeView, GameRecord, GameSource, GameSourceRef, MoveView,
};
use super::{GameIndex, display_label};

/// 棋譜プレイヤー TUI を起動する。`Ctrl-C`／`q` で終了するまでブロックする。
pub fn run(source: Box<dyn GameSource>) -> Result<()> {
    let index = source.build_index()?;
    for warning in &index.warnings {
        eprintln!("warning: {warning}");
    }
    if index.entries.is_empty() {
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

    let mut app = App::new(source, index);
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
    /// ファイル列挙順→完了順（従来のデフォルト）。
    Discovery,
    /// エラー→黒勝ち→白勝ち→引き分け→不明の順にグルーピング。
    Outcome,
    /// JSONL のペアファイル（`file_idx`）単位でグルーピング。PSV は全件 1 グループ。
    EnginePair,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::Discovery => SortMode::Outcome,
            SortMode::Outcome => SortMode::EnginePair,
            SortMode::EnginePair => SortMode::Discovery,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::Discovery => "発見順",
            SortMode::Outcome => "勝敗別",
            SortMode::EnginePair => "エンジンペア別",
        }
    }
}

struct App {
    source: Box<dyn GameSource>,
    index: GameIndex,
    /// `index.entries` のうち、現在のフィルタに一致するものの index 列。
    filtered: Vec<usize>,
    selected: usize,
    mode: Mode,
    filter_input: String,
    sort_mode: SortMode,
    current_game: Option<GameRecord>,
    current_move: usize,
    status: String,
}

impl App {
    fn new(source: Box<dyn GameSource>, index: GameIndex) -> Self {
        let filtered: Vec<usize> = (0..index.entries.len()).collect();
        let mut app = Self {
            source,
            index,
            filtered,
            selected: 0,
            mode: Mode::Browse,
            filter_input: String::new(),
            sort_mode: SortMode::Discovery,
            current_game: None,
            current_move: 0,
            status: String::new(),
        };
        app.load_selected();
        app
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
                self.current_game = Some(game);
            }
            Err(e) => self.status = format!("対局の読み込みに失敗しました: {e}"),
        }
    }

    fn apply_filter(&mut self) {
        let query = self.filter_input.to_lowercase();
        let filter = parse_filter(&query);
        let mut filtered: Vec<usize> = (0..self.index.entries.len())
            .filter(|&i| entry_matches(&self.index, &self.index.entries[i], filter))
            .collect();
        sort_filtered(&mut filtered, &self.index.entries, self.sort_mode);
        self.filtered = filtered;
        self.selected = 0;
        self.load_selected();
    }

    fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.apply_filter();
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
                KeyCode::Enter => self.mode = Mode::Browse,
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
                KeyCode::Char('h') | KeyCode::Left => self.prev_move(),
                KeyCode::Char('l') | KeyCode::Right => self.next_move(),
                KeyCode::Char('j') | KeyCode::Down => self.next_game(),
                KeyCode::Char('k') | KeyCode::Up => self.prev_game(),
                KeyCode::Char('n') => self.jump_to_next_eval_swing(),
                KeyCode::Char('N') => self.jump_to_prev_eval_swing(),
                KeyCode::Char('s') => self.cycle_sort_mode(),
                KeyCode::Char('/') => self.mode = Mode::Filter,
                KeyCode::Char('?') => self.mode = Mode::Help,
                _ => {}
            },
        }
        true
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
        GameSourceRef::Psv { .. } => None,
    }
}

/// `/` 検索クエリを解析した結果。判定ロジックは `entry_matches` に集約する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter<'a> {
    Empty,
    /// `pair:4` 等の `field:value` 構文。指定フィールドの完全一致のみを見る。
    Field(FieldKind, &'a str),
    /// prefix 無しで数字のみのクエリ。ラベル部分一致を無効化し、数値フィールドの
    /// 完全一致のみを見る（`vol4B_raw` のようにラベルに数字を含むデータで
    /// `pair_index=4` のつもりの `"4"` がラベルにも部分一致してしまう問題への対応）。
    NumericExact(&'a str),
    /// それ以外の自由文字列。従来どおりラベル部分一致 OR outcome キーワード部分一致。
    Text(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Pair,
    Slot,
    Startpos,
    Id,
    Outcome,
    Label,
}

/// 検索クエリを解析する。`query` は呼び出し側で既に小文字化済みの前提。
fn parse_filter(query: &str) -> Filter<'_> {
    if query.is_empty() {
        return Filter::Empty;
    }
    if let Some((prefix, value)) = query.split_once(':') {
        let field = match prefix {
            "pair" => Some(FieldKind::Pair),
            "slot" => Some(FieldKind::Slot),
            "startpos" => Some(FieldKind::Startpos),
            "id" => Some(FieldKind::Id),
            "outcome" => Some(FieldKind::Outcome),
            "label" => Some(FieldKind::Label),
            _ => None,
        };
        if let Some(field) = field {
            return Filter::Field(field, value);
        }
    }
    if query.bytes().all(|b| b.is_ascii_digit()) {
        return Filter::NumericExact(query);
    }
    Filter::Text(query)
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
    }
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
fn sort_filtered(filtered: &mut [usize], entries: &[GameIndexEntry], mode: SortMode) {
    match mode {
        SortMode::Discovery => {}
        SortMode::Outcome => filtered.sort_by_key(|&i| outcome_sort_key(&entries[i])),
        SortMode::EnginePair => {
            filtered.sort_by_key(|&i| entries[i].file_idx().unwrap_or(usize::MAX))
        }
    }
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if let Event::Key(key) = event::read()?
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

    // 盤面は内容ぴったりの固定幅にし、余りは指し手パネルへ回す（盤面を広い割合で
    // 確保すると右側が広大なデッドゾーンになるため）。盤面の最大行幅は罫線46＋段
    // ラベル3＋Block枠2＝51桁なので 52 を確保する。
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Length(52),
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
            ListItem::new(format!("{label}{marker}"))
        })
        .collect();

    let title = format!(
        "対局一覧 ({}/{}) [{}]",
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
        Some(mv) => render_board(&mv.sfen_before, mv.mv),
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

/// `game.moves` と同じ長さ・同じ並びの打点列（評価値が無い手は `None`）。
/// 「手」のインデックスで隣接判定するために、評価値の有無でフィルタした
/// flat なリストにはしない（フィルタ後に隣接させると、評価値が欠けた手を
/// 挟んだ前後の手が直線で繋がってしまい、欠損が無かったように見えてしまう）。
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
    let aligned = eval_points(game);
    let plotted: Vec<(f64, f64, Color)> = aligned.iter().filter_map(|p| *p).collect();
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
            // 着手側で色分けした線分（着手後の評価値を、その着手側の色で結ぶ）。
            // 評価値の無い手を挟む区間は線を引かない（隣接する「手」同士のみ結ぶ）。
            for pair in aligned.windows(2) {
                let (Some((x1, y1, _)), Some((x2, y2, side2))) = (pair[0], pair[1]) else {
                    continue;
                };
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
    let text = match &app.mode {
        Mode::Filter => format!("検索: {}_", app.filter_input),
        Mode::Help => "何かキーを押すとヘルプを閉じます".to_string(),
        Mode::Browse => {
            // ヘルプは常に行頭に固定する（手を動かしても位置がずれないよう、可変長の
            // 注釈はここに出さず指し手パネル側へ移した）。
            let help = format!(
                "h/l:手  j/k:対局  n/N:評価値急変  s:並替({})  /:検索  ?:ヘルプ  q:終了",
                app.sort_mode.label()
            );
            if app.status.is_empty() {
                format!("[{help}]")
            } else {
                format!("[{help}]   {}", app.status)
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
        Line::from("n        次の評価値急変手へジャンプ"),
        Line::from("N        前の評価値急変手へジャンプ"),
        Line::from(format!(
            "s        対局リストの並べ替えを切り替え（{}/{}/{}）",
            SortMode::Discovery.label(),
            SortMode::Outcome.label(),
            SortMode::EnginePair.label()
        )),
        Line::from("/        検索・フィルタ入力（Enter/Esc で終了、Esc はクリアも兼ねる）"),
        Line::from("?        このヘルプの表示・終了"),
        Line::from("q / Esc  終了（ヘルプ表示中は閉じるだけ）"),
        Line::from(""),
        Line::from("検索構文: pair:<n> slot:<n> startpos:<n> id:<n> outcome:<kw> label:<text>"),
        Line::from("prefix 無しで数字のみを入力すると、上記フィールドの完全一致のみで絞り込みます"),
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

/// 指し手行に並べる注釈（評価値・探索情報）。engine ラベルは対局ラベルと重複して
/// 冗長なので出さない。注釈が無い手は空文字を返す（行に何も足さない）。
fn annotation_inline(mv: &MoveView) -> String {
    let a = &mv.annotation;
    let mut parts = Vec::new();
    if let Some(v) = a.score_mate {
        parts.push(format!("詰み{v:+}"));
    } else if let Some(v) = a.score_cp {
        parts.push(format!("評価値{v:+}"));
    }
    if let Some(v) = a.depth {
        parts.push(format!("depth{v}"));
    }
    if let Some(v) = a.seldepth {
        parts.push(format!("seldepth{v}"));
    }
    if let Some(v) = a.nodes {
        parts.push(format!("nodes{v}"));
    }
    if let Some(v) = a.nps {
        parts.push(format!("nps{v}"));
    }
    if let Some(v) = a.elapsed_ms {
        parts.push(format!("経過{v}ms"));
    }
    // think_limit_ms（制限）は出さない：depth 制限の対局では書き込み側が既定値を入れる
    // ため実際の制御（例: depth 15）と乖離して誤解を招く。実消費は 経過 が、時間切れは
    // TIMEOUT が表す。
    if a.timed_out == Some(true) {
        parts.push("TIMEOUT".to_string());
    }
    parts.join(" ")
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

fn render_board(sfen: &str, mv: Move) -> Vec<Line<'static>> {
    let mut pos = Position::new();
    if pos.set_sfen(sfen).is_err() {
        return vec![Line::from("(局面を表示できません)")];
    }

    // `sfen` は着手前の局面。通常手・駒打ち（is_normal）は指了後の局面を表示するため
    // mv を適用し、移動先（駒）と移動元（空マス）をハイライトする。手番・王手・持駒も
    // 適用後を反映する（王手表示＝最終手が王手だったこと）。記録側は合法手のみ USI で
    // 書き、終局・不正手は Move::from_usi が None → Move::NONE になるため、適用する mv は
    // sfen_before に対し必ず合法で do_move は安全。pass 手・終局擬似手（is_normal=false）は
    // 適用しない（pass 権 state を持たない局面で do_pass_move が panic するため）ので、
    // その場合は記録局面（指了前）のまま表示する。
    let (highlight_from, highlight_to) = move_highlight_squares(mv);
    if mv.is_normal() {
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
    use super::super::model::MoveAnnotation;
    use super::*;

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
        }
    }

    fn empty_index() -> GameIndex {
        GameIndex::default()
    }

    #[test]
    fn black_pov_cp_keeps_sign_for_black_mover() {
        // 先手が指した手で score_cp=+120（先手にとって +120）なら、
        // グラフ用の先手 POV もそのまま +120（先手優勢）。
        assert_eq!(black_pov_cp(&mv(Color::Black, Some(120), None)), Some(120.0));
    }

    #[test]
    fn black_pov_cp_flips_sign_for_white_mover() {
        // 後手が指した手で score_cp=+80（後手にとって +80 = 後手優勢）なら、
        // 先手 POV では -80（後手優勢はマイナスで表す）。
        assert_eq!(black_pov_cp(&mv(Color::White, Some(80), None)), Some(-80.0));
    }

    #[test]
    fn black_pov_cp_clamps_and_keeps_sign_for_mate() {
        // 後手が指した手で詰みあり（後手が詰ます = 後手にとって正の mate）なら、
        // 先手 POV では負の sentinel（後手優勢）。
        assert_eq!(black_pov_cp(&mv(Color::White, None, Some(3))), Some(-GRAPH_CP_CLAMP));
        // 先手が指した手で詰みあり（先手が詰まされる = 負の mate）なら、
        // 先手 POV でも負の sentinel（後手優勢）のまま。
        assert_eq!(black_pov_cp(&mv(Color::Black, None, Some(-2))), Some(-GRAPH_CP_CLAMP));
    }

    #[test]
    fn black_pov_cp_none_when_no_eval() {
        assert_eq!(black_pov_cp(&mv(Color::Black, None, None)), None);
    }

    #[test]
    fn eval_points_preserves_gap_position_for_missing_eval() {
        // 中央の手だけ評価値が無い対局。draw_eval_graph 側はこの None を
        // 「前後の手を直線で繋がない」境界として使うため、None の位置が
        // 元の手の並びと一致していることをここで固定する。
        let game = GameRecord {
            moves: vec![
                mv(Color::Black, Some(10), None),
                mv(Color::White, None, None),
                mv(Color::Black, Some(-5), None),
            ],
            leading_gap_is_drop: false,
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
        assert_eq!(SortMode::Discovery.next(), SortMode::Outcome);
        assert_eq!(SortMode::Outcome.next(), SortMode::EnginePair);
        assert_eq!(SortMode::EnginePair.next(), SortMode::Discovery);
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
        sort_filtered(&mut filtered, &entries, SortMode::Outcome);
        // error(2) → b-win(1,3、発見順維持) → draw(0)
        assert_eq!(filtered, vec![2, 1, 3, 0]);
    }

    #[test]
    fn sort_filtered_by_engine_pair_groups_by_file_idx() {
        let entries = vec![
            jsonl_entry(1, None, None, None, None, false, 1),
            jsonl_entry(2, None, None, None, None, false, 0),
            jsonl_entry(3, None, None, None, None, false, 1),
            jsonl_entry(4, None, None, None, None, false, 0),
        ];
        let mut filtered: Vec<usize> = (0..entries.len()).collect();
        sort_filtered(&mut filtered, &entries, SortMode::EnginePair);
        assert_eq!(filtered, vec![1, 3, 0, 2]);
    }

    #[test]
    fn sort_filtered_discovery_mode_is_identity() {
        let entries = vec![
            jsonl_entry(1, None, None, None, None, true, 0),
            jsonl_entry(2, None, None, None, None, false, 0),
        ];
        let mut filtered: Vec<usize> = (0..entries.len()).collect();
        sort_filtered(&mut filtered, &entries, SortMode::Discovery);
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
        };
        assert_eq!(ply_gap_before(&game, 0), None, "定跡開始は先頭欠番扱いにしない");
        assert_eq!(ply_gap_before(&game, 1), None);
    }

    #[test]
    fn annotation_inline_omits_engine_and_is_empty_without_data() {
        let mut m = mv_with_ply(1, Color::Black, None, None);
        assert_eq!(annotation_inline(&m), "", "注釈が無ければ空文字（行に何も足さない）");
        m.annotation = MoveAnnotation {
            score_cp: Some(-77),
            depth: Some(15),
            engine_label: Some("vol4B_nnued15_30m".to_string()),
            ..Default::default()
        };
        let s = annotation_inline(&m);
        assert!(s.contains("評価値-77") && s.contains("depth15"), "評価値と探索情報を出す: {s}");
        assert!(
            !s.contains("engine") && !s.contains("vol4B"),
            "engine は冗長なので出さない: {s}"
        );
    }

    #[test]
    fn ply_gap_before_none_for_consecutive_plies() {
        let game = GameRecord {
            moves: vec![
                mv_with_ply(1, Color::Black, None, None),
                mv_with_ply(2, Color::White, None, None),
            ],
            leading_gap_is_drop: false,
        };
        assert_eq!(ply_gap_before(&game, 1), None);
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
        let after = joined(&render_board(HIRATE, mv));
        let none = joined(&render_board(HIRATE, Move::NONE));
        assert_ne!(after, none, "通常手は do_move で盤面に反映される");
        assert!(!after.contains("先手番") && after.contains("後手番"), "指了後は後手番");
        assert!(none.contains("先手番"), "Move::NONE は do_move せず手番も変わらない");
    }

    #[test]
    fn render_board_shows_promoted_pieces_as_single_char_glyph() {
        // 各成り駒を1つずつ置いた局面（Move::NONE なので do_move しない）。
        let sfen = "3k5/9/9/9/+P+L+N+S+B+R3/9/9/9/3K5 b - 1";
        let s = joined(&render_board(sfen, Move::NONE));
        for g in ["と", "杏", "圭", "全", "馬", "龍"] {
            assert!(s.contains(g), "成り駒 {g} を一文字グリフで表示");
        }
        assert!(!s.contains('成') && !s.contains('+'), "盤面に 成/+ の生表記は出さない");
    }

    #[test]
    fn render_board_unparsable_sfen_shows_placeholder() {
        let lines = render_board("not-a-sfen", Move::NONE);
        assert_eq!(joined(&lines), "(局面を表示できません)");
    }
}
