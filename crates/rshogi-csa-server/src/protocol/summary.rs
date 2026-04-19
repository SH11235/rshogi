//! `Game_Summary` ブロックの生成。
//!
//! CSA v1.2.1 の `BEGIN Game_Summary` ... `END Game_Summary` を組み立てる。
//! 各対局者宛ての出力は `Your_Turn` だけが異なるため、ビルダ 1 つから
//! [`GameSummaryBuilder::build_for`] を Color 別に呼び分ける。

use std::fmt::Write as _;

use crate::types::{Color, GameId, PlayerName};

/// `Game_Summary` の入力パラメタ。
#[derive(Debug, Clone)]
pub struct GameSummaryBuilder {
    /// 対局 ID。
    pub game_id: GameId,
    /// 先手プレイヤ名。
    pub black: PlayerName,
    /// 後手プレイヤ名。
    pub white: PlayerName,
    /// 持ち時間セクション（`BEGIN Time` から `END Time` まで、末尾改行込み）。
    /// 通常は [`crate::game::clock::TimeClock::format_summary`] の戻り値を渡す。
    pub time_section: String,
    /// 初期局面ブロック（`BEGIN Position` … `END Position` 全体、末尾改行込み）。
    /// 平手平局面なら標準のブロックを文字列で渡す（builder 自身は組み立てない）。
    pub position_section: String,
    /// 引き分け再対局可否。CSA 仕様では `Rematch_On_Draw:NO` 既定。
    pub rematch_on_draw: bool,
    /// 開始時の手番。CSA 仕様 `To_Move:` に直接書ける `+`/`-` 文字。
    pub to_move: Color,
    /// 入玉宣言ルール表示（`Declaration:Jishogi 1.1` など）。空ならデフォルト省略。
    pub declaration: String,
}

impl GameSummaryBuilder {
    /// `you` 宛ての Game_Summary 文字列を組み立てる。
    ///
    /// `Your_Turn:` は `you` の色に応じて `+`/`-` を出力する。
    pub fn build_for(&self, you: Color) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("BEGIN Game_Summary\n");
        out.push_str("Protocol_Version:1.2\n");
        out.push_str("Protocol_Mode:Server\n");
        out.push_str("Format:Shogi 1.0\n");
        if !self.declaration.is_empty() {
            let _ = writeln!(out, "Declaration:{}", self.declaration);
        }
        let _ = writeln!(out, "Game_ID:{}", self.game_id);
        let _ = writeln!(out, "Name+:{}", self.black);
        let _ = writeln!(out, "Name-:{}", self.white);
        let _ = writeln!(out, "Your_Turn:{}", color_char(you));
        let _ =
            writeln!(out, "Rematch_On_Draw:{}", if self.rematch_on_draw { "YES" } else { "NO" });
        let _ = writeln!(out, "To_Move:{}", color_char(self.to_move));
        // 持ち時間セクションは TimeClock 由来の文字列をそのまま埋め込む。
        out.push_str(&self.time_section);
        if !self.time_section.ends_with('\n') {
            out.push('\n');
        }
        // 初期局面セクション（`BEGIN Position`...`END Position` 全体）。
        out.push_str(&self.position_section);
        if !self.position_section.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("END Game_Summary\n");
        out
    }
}

fn color_char(c: Color) -> char {
    match c {
        Color::Black => '+',
        Color::White => '-',
    }
}

/// 平手初期局面の `BEGIN Position`...`END Position` ブロックを返す。
///
/// `KifuRecord` でも使えるよう、CSA 標準の P1-P9 + 持ち駒なし + 手番（`+`）を
/// 1 つの文字列として返す。駒落ち対応時は別経路（PI 行や P+/P- 駒配置）を
/// 追加することになる。
pub fn standard_initial_position_block() -> String {
    // rshogi-csa::initial_position().to_csa_board() がそのまま使えるが、
    // ここで `BEGIN Position`/`END Position` で囲んで返す。
    let board = rshogi_csa::initial_position().to_csa_board();
    let mut out = String::with_capacity(board.len() + 32);
    out.push_str("BEGIN Position\n");
    out.push_str(&board);
    if !board.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("END Position\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skeleton() -> GameSummaryBuilder {
        GameSummaryBuilder {
            game_id: GameId::new("20140101120000"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            time_section: "BEGIN Time\nTime_Unit:1sec\nTotal_Time:600\nByoyomi:10\nLeast_Time_Per_Move:0\nEND Time\n".to_owned(),
            position_section: standard_initial_position_block(),
            rematch_on_draw: false,
            to_move: Color::Black,
            declaration: "Jishogi 1.1".to_owned(),
        }
    }

    #[test]
    fn build_for_black_emits_your_turn_plus() {
        let txt = skeleton().build_for(Color::Black);
        assert!(txt.starts_with("BEGIN Game_Summary\n"));
        assert!(txt.contains("\nYour_Turn:+\n"));
        assert!(txt.ends_with("END Game_Summary\n"));
    }

    #[test]
    fn build_for_white_emits_your_turn_minus() {
        let txt = skeleton().build_for(Color::White);
        assert!(txt.contains("\nYour_Turn:-\n"));
    }

    #[test]
    fn build_for_includes_required_csa_fields_in_order() {
        let txt = skeleton().build_for(Color::Black);
        let pos = |needle: &str| txt.find(needle).unwrap_or_else(|| panic!("missing: {needle}"));
        // 必須フィールドが期待順で出る。
        let pv = pos("Protocol_Version:1.2");
        let pm = pos("Protocol_Mode:Server");
        let fmt = pos("Format:Shogi 1.0");
        let decl = pos("Declaration:Jishogi 1.1");
        let gid = pos("Game_ID:20140101120000");
        let name_p = pos("Name+:alice");
        let name_m = pos("Name-:bob");
        let your = pos("Your_Turn:+");
        let rematch = pos("Rematch_On_Draw:NO");
        let to_move = pos("To_Move:+");
        let begin_time = pos("BEGIN Time");
        let begin_pos = pos("BEGIN Position");
        let end_pos = pos("END Position");
        assert!(pv < pm);
        assert!(pm < fmt);
        assert!(fmt < decl);
        assert!(decl < gid);
        assert!(gid < name_p);
        assert!(name_p < name_m);
        assert!(name_m < your);
        assert!(your < rematch);
        assert!(rematch < to_move);
        assert!(to_move < begin_time);
        assert!(begin_time < begin_pos);
        assert!(begin_pos < end_pos);
    }

    #[test]
    fn declaration_is_optional() {
        let mut b = skeleton();
        b.declaration = String::new();
        let txt = b.build_for(Color::Black);
        assert!(!txt.contains("Declaration:"));
    }

    #[test]
    fn rematch_yes_when_flag_set() {
        let mut b = skeleton();
        b.rematch_on_draw = true;
        let txt = b.build_for(Color::Black);
        assert!(txt.contains("Rematch_On_Draw:YES"));
    }

    #[test]
    fn standard_initial_position_block_format() {
        let block = standard_initial_position_block();
        assert!(block.starts_with("BEGIN Position\n"));
        assert!(block.contains("P1-KY"));
        assert!(block.contains("P9+KY"));
        assert!(block.ends_with("END Position\n"));
    }
}
