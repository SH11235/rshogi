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
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType, Square};

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
}

struct App {
    source: Box<dyn GameSource>,
    index: GameIndex,
    /// `index.entries` のうち、現在のフィルタに一致するものの index 列。
    filtered: Vec<usize>,
    selected: usize,
    mode: Mode,
    filter_input: String,
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
            Ok(game) => self.current_game = Some(game),
            Err(e) => self.status = format!("対局の読み込みに失敗しました: {e}"),
        }
    }

    fn apply_filter(&mut self) {
        let query = self.filter_input.to_lowercase();
        self.filtered = (0..self.index.entries.len())
            .filter(|&i| entry_matches(&self.index, &self.index.entries[i], &query))
            .collect();
        self.selected = 0;
        self.load_selected();
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

    /// `false` を返したらイベントループを終了する。
    fn handle_key(&mut self, code: KeyCode) -> bool {
        match self.mode {
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
                KeyCode::Char('/') => self.mode = Mode::Filter,
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

fn entry_matches(index: &GameIndex, entry: &GameIndexEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if display_label(index, entry).to_lowercase().contains(query) {
        return true;
    }
    if outcome_keyword(entry).contains(query) {
        return true;
    }
    if let GameSourceRef::Jsonl { game_id, .. } = entry.source
        && game_id.to_string() == query
    {
        return true;
    }
    [entry.pair_index, entry.pair_slot, entry.startpos_idx]
        .iter()
        .flatten()
        .any(|v| v.to_string() == query)
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
            Constraint::Min(10),
            Constraint::Length(9),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
        ])
        .split(root[0]);

    draw_game_list(frame, app, main[0]);
    draw_board(frame, app, main[1]);
    draw_move_list(frame, app, main[2]);
    draw_eval_graph(frame, app, root[1]);
    draw_status_bar(frame, app, root[2]);
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

    let title = format!("対局一覧 ({}/{})", app.filtered.len(), app.index.entries.len());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if !app.filtered.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_board(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let lines = match current_move(app) {
        Some(mv) => render_board(&mv.sfen_before),
        None => vec![Line::from("(対局を選択してください)")],
    };
    let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("盤面"));
    frame.render_widget(para, area);
}

fn draw_move_list(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = match &app.current_game {
        Some(game) => game.moves.iter().map(|mv| ListItem::new(mv.kif_label.clone())).collect(),
        None => Vec::new(),
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("指し手"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if app.current_game.is_some() {
        state.select(Some(app.current_move));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// 評価値グラフの Y 軸クランプ幅（cp 換算）。詰みはこの符号付き値に丸める。
const GRAPH_CP_CLAMP: f64 = 3000.0;

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

/// `(ply, black_pov_cp, mover)` の点列。X 軸には実際の `ply` を使う
/// （PSV の `skip_initial_ply`/`skip_in_check` による欠番がそのまま見えるように）。
fn eval_points(game: &GameRecord) -> Vec<(f64, f64, Color)> {
    game.moves
        .iter()
        .filter_map(|mv| black_pov_cp(mv).map(|cp| (mv.ply as f64, cp, mv.side)))
        .collect()
}

fn draw_eval_graph(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("評価値グラフ（＋先手優勢／－後手優勢）");
    let Some(game) = &app.current_game else {
        frame.render_widget(Paragraph::new("(対局を選択してください)").block(block), area);
        return;
    };
    let points = eval_points(game);
    if points.len() < 2 {
        frame.render_widget(Paragraph::new("(表示できる評価値がありません)").block(block), area);
        return;
    }

    let min_ply = points.first().map(|p| p.0).unwrap_or(0.0);
    let max_ply = points.last().map(|p| p.0).unwrap_or(1.0).max(min_ply + 1.0);
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
            for pair in points.windows(2) {
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
    let text = match &app.mode {
        Mode::Filter => format!("検索: {}_", app.filter_input),
        Mode::Browse => {
            let annotation = current_move(app).map(annotation_line).unwrap_or_default();
            let help = "h/l:手  j/k:対局  /:検索  q:終了";
            if app.status.is_empty() {
                format!("{annotation}   [{help}]")
            } else {
                format!("{}   [{help}]", app.status)
            }
        }
    };
    let para = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, area);
}

fn current_move(app: &App) -> Option<&MoveView> {
    app.current_game.as_ref().and_then(|g| g.moves.get(app.current_move))
}

fn annotation_line(mv: &MoveView) -> String {
    let a = &mv.annotation;
    let mut parts = Vec::new();
    if let Some(v) = a.score_mate {
        parts.push(format!("詰み{v:+}"));
    } else if let Some(v) = a.score_cp {
        parts.push(format!("評価値{v:+}"));
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
        parts.push(format!("経過={v}ms"));
    }
    if let Some(v) = a.think_limit_ms {
        parts.push(format!("制限={v}ms"));
    }
    if a.timed_out == Some(true) {
        parts.push("TIMEOUT".to_string());
    }
    if let Some(v) = &a.engine_label {
        parts.push(format!("engine={v}"));
    }
    if parts.is_empty() {
        "(注釈なし)".to_string()
    } else {
        parts.join("  ")
    }
}

fn render_board(sfen: &str) -> Vec<Line<'static>> {
    let mut pos = Position::new();
    if pos.set_sfen(sfen).is_err() {
        return vec![Line::from("(局面を表示できません)")];
    }

    let mut lines = Vec::new();
    let turn = if pos.side_to_move() == Color::Black {
        "先手番"
    } else {
        "後手番"
    };
    lines.push(Line::from(format!("手番: {turn}")));
    lines.push(Line::from(format!("後手持駒: {}", hand_text(&pos, Color::White))));
    lines.push(Line::from(""));

    for rank in 0..9u8 {
        let mut spans = Vec::new();
        for file in (0..9u8).rev() {
            let sq_idx = file * 9 + rank;
            let Some(sq) = Square::from_u8(sq_idx) else {
                continue;
            };
            let piece = pos.piece_on(sq);
            if piece.is_none() {
                spans.push(Span::raw(" ・"));
                continue;
            }
            let label = piece_label(piece.piece_type(), piece.piece_type().is_promoted());
            let style = if piece.color() == Color::Black {
                Style::default().fg(RColor::Yellow)
            } else {
                Style::default().fg(RColor::Cyan)
            };
            spans.push(Span::styled(format!("{label:>2}"), style));
        }
        lines.push(Line::from(spans));
    }

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
        MoveView {
            ply: 1,
            side,
            sfen_before: String::new(),
            mv: rshogi_core::types::Move::NONE,
            kif_label: String::new(),
            annotation: MoveAnnotation {
                score_cp,
                score_mate,
                ..Default::default()
            },
        }
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
}
