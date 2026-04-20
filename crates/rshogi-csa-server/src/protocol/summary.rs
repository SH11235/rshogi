//! `Game_Summary` ブロックの生成。
//!
//! CSA v1.2.1 の `BEGIN Game_Summary` ... `END Game_Summary` を組み立てる。
//! 各対局者宛ての出力は `Your_Turn` だけが異なるため、ビルダ 1 つから
//! [`GameSummaryBuilder::build_for`] を Color 別に呼び分ける。

use std::fmt::Write as _;

use rshogi_core::position::Position;
use rshogi_core::types::{Color as CoreColor, File, PieceType, Rank, Square};

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

/// 任意 SFEN から手番色 ([`Color`]) を抽出する。
///
/// Game_Summary の `To_Move:` フィールドを `initial_sfen` から派生させるための
/// ヘルパ。`GameRoomConfig::initial_sfen` が `Some(sfen)` の場合、フロントエンド
/// はこの関数で手番を取得し `GameSummaryBuilder::to_move` に渡すことで、
/// `GameRoom` / Game_Summary / 棋譜の三点一致を保つ。
///
/// # Errors
/// 不正 SFEN なら文字列エラー。
pub fn side_to_move_from_sfen(sfen: &str) -> Result<Color, String> {
    let mut pos = Position::new();
    pos.set_sfen(sfen)
        .map_err(|e| format!("invalid initial_sfen {sfen:?}: {e:?}"))?;
    Ok(match pos.side_to_move() {
        CoreColor::Black => Color::Black,
        CoreColor::White => Color::White,
    })
}

/// 任意 SFEN から `BEGIN Position`...`END Position` ブロックを組み立てる。
///
/// - Game_Summary の `position_section` と、棋譜 ([`crate::record::kifu::KifuRecord::initial_position`])
///   の両方で **同一 SFEN から派生させる契約** を満たすために一本化した入口。
/// - `rshogi_core::Position::set_sfen` で SFEN を検証・展開した上で、CSA の
///   P1-P9 / P+ / P- / 手番行を自力で組み立てる。
///
/// # Errors
/// 渡された SFEN が `rshogi_core` で不正と判定された場合はエラー文字列を返す。
pub fn position_section_from_sfen(sfen: &str) -> Result<String, String> {
    let mut pos = Position::new();
    pos.set_sfen(sfen)
        .map_err(|e| format!("invalid initial_sfen {sfen:?}: {e:?}"))?;
    Ok(position_section_from_position(&pos))
}

/// 既に展開済みの [`Position`] から `BEGIN Position`...`END Position` ブロックを組み立てる。
///
/// `position_section_from_sfen` の内部実装。`GameRoom` が自分の `pos` から
/// 直接 position_section を作る際にも使える。
pub fn position_section_from_position(pos: &Position) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("BEGIN Position\n");

    // P1-P9: CSA の 1 行は「P<rank>」に続けて **file 9 (左) → file 1 (右)** の順で
    // 3 文字ずつ (駒は `+FU` / `-HI` 等、空升は ` * `) を並べる。
    for rank_idx in 0..9u8 {
        let rank = Rank::from_u8(rank_idx).expect("rank 0..9");
        let _ = write!(out, "P{}", rank_idx + 1);
        for file_idx in (0..9u8).rev() {
            let file = File::from_u8(file_idx).expect("file 0..9");
            let sq = Square::new(file, rank);
            let pc = pos.piece_on(sq);
            if pc.is_none() {
                out.push_str(" * ");
            } else {
                let side = match pc.color() {
                    CoreColor::Black => '+',
                    CoreColor::White => '-',
                };
                let code = csa_piece_code(pc.piece_type());
                out.push(side);
                out.push_str(code);
            }
        }
        out.push('\n');
    }

    // P+ / P- 持ち駒行: 枚数を展開して「00<駒種>」を繰り返す。
    // 枚数順は CSA 慣用の「飛 → 角 → 金 → 銀 → 桂 → 香 → 歩」。
    append_hand_line(&mut out, "P+", pos.hand(CoreColor::Black));
    append_hand_line(&mut out, "P-", pos.hand(CoreColor::White));

    // 手番行。
    let side_char = match pos.side_to_move() {
        CoreColor::Black => '+',
        CoreColor::White => '-',
    };
    out.push(side_char);
    out.push('\n');

    out.push_str("END Position\n");
    out
}

fn append_hand_line(out: &mut String, prefix: &str, hand: rshogi_core::types::Hand) {
    let entries: &[PieceType] = &[
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ];
    let total: u32 = entries.iter().map(|pt| hand.count(*pt)).sum();
    if total == 0 {
        return;
    }
    out.push_str(prefix);
    for pt in entries {
        let n = hand.count(*pt);
        for _ in 0..n {
            out.push_str("00");
            out.push_str(csa_piece_code(*pt));
        }
    }
    out.push('\n');
}

/// `PieceType` を CSA 2 文字コードへ変換する。`KY` などはバリアント 14 種分網羅。
fn csa_piece_code(pt: PieceType) -> &'static str {
    match pt {
        PieceType::Pawn => "FU",
        PieceType::Lance => "KY",
        PieceType::Knight => "KE",
        PieceType::Silver => "GI",
        PieceType::Gold => "KI",
        PieceType::Bishop => "KA",
        PieceType::Rook => "HI",
        PieceType::King => "OU",
        PieceType::ProPawn => "TO",
        PieceType::ProLance => "NY",
        PieceType::ProKnight => "NK",
        PieceType::ProSilver => "NG",
        PieceType::Horse => "UM",
        PieceType::Dragon => "RY",
    }
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

    #[test]
    fn position_section_from_sfen_equals_standard_block_for_hirate() {
        // 平手 SFEN で `position_section_from_sfen` を呼んだ結果は、既存の
        // `standard_initial_position_block()` とほぼ一致するはず (差は無い想定)。
        // 完全一致は駒並びや手番・持ち駒なしの表現次第だが、本実装では両者とも
        // `PI` 行を使わず P1-P9 + 手番のみなので全行一致する。
        let hirate = rshogi_core::position::SFEN_HIRATE;
        let block = position_section_from_sfen(hirate).unwrap();
        let std_block = standard_initial_position_block();
        assert_eq!(block, std_block);
    }

    #[test]
    fn position_section_from_sfen_rejects_invalid_sfen() {
        let err = position_section_from_sfen("not-a-sfen").unwrap_err();
        assert!(err.contains("invalid initial_sfen"), "unexpected: {err}");
    }

    #[test]
    fn position_section_from_sfen_emits_side_to_move_minus_for_white() {
        // Validator の oute-sennichite Win SFEN は side=White。position_section 末尾の
        // 手番行も `-` になることを固定する。
        let block = position_section_from_sfen("9/6k2/9/9/9/9/9/6R2/K8 w - 1").unwrap();
        assert!(block.contains("\n-\nEND Position"));
    }

    #[test]
    fn position_section_from_sfen_emits_hand_lines_for_27pt_sfen() {
        // 27 点法 SFEN は先手手駒に RB (飛・角) を保持する。P+ 行に `00HI00KA`
        // が出ることを確認する。
        let block = position_section_from_sfen("LNSGKGSNL/4BR3/9/9/9/9/9/9/4k4 b RB 1").unwrap();
        assert!(block.contains("P+00HI00KA"), "block missing P+ hand: {block}");
        // White 手駒は空なので P- 行は出ない。
        assert!(!block.contains("P-"), "block should not have P- line: {block}");
    }
}
