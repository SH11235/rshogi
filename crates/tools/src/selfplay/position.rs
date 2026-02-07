use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use rshogi_core::movegen::is_legal_with_pass;
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::types::Move;

/// USI position 行を分解した結果。
pub struct ParsedPosition {
    pub startpos: bool,
    pub sfen: Option<String>,
    pub moves: Vec<String>,
}

/// 開始局面群をファイル / 単一SFEN / デフォルト(平手) からロードする。
pub fn load_start_positions(
    file: Option<&Path>,
    sfen: Option<&str>,
    pass_rights_black: Option<u8>,
    pass_rights_white: Option<u8>,
) -> Result<(Vec<ParsedPosition>, Vec<String>)> {
    match (file, sfen) {
        (Some(_), Some(_)) => {
            bail!("--startpos-file and --sfen cannot be used together");
        }
        (Some(path), None) => {
            let file = std::fs::File::open(path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            let reader = BufReader::new(file);
            let mut positions = Vec::new();
            let mut commands = Vec::new();
            for (idx, line) in reader.lines().enumerate() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                // position形式または生のSFEN形式の両方をサポート
                let parsed = parse_position_line(trimmed)
                    .or_else(|_| parse_sfen_only(trimmed))
                    .with_context(|| {
                        format!("invalid position syntax on line {}: {}", idx + 1, trimmed)
                    })?;
                build_position(&parsed, pass_rights_black, pass_rights_white)?;
                let cmd = describe_position(&parsed);
                positions.push(parsed);
                commands.push(cmd);
            }
            if positions.is_empty() {
                bail!("no usable positions found in {}", path.display());
            }
            Ok((positions, commands))
        }
        (None, Some(sfen_arg)) => {
            let parsed = parse_position_line(sfen_arg).or_else(|_| parse_sfen_only(sfen_arg))?;
            build_position(&parsed, pass_rights_black, pass_rights_white)?;
            let cmd = describe_position(&parsed);
            Ok((vec![parsed], vec![cmd]))
        }
        (None, None) => {
            let parsed = ParsedPosition {
                startpos: true,
                sfen: None,
                moves: Vec::new(),
            };
            Ok((vec![parsed], vec!["position startpos".to_string()]))
        }
    }
}

/// `position ...` 形式の行をパースする。
pub fn parse_position_line(line: &str) -> Result<ParsedPosition> {
    let mut tokens = line.split_whitespace().peekable();
    if tokens.peek().is_some_and(|tok| *tok == "position") {
        tokens.next();
    }
    match tokens.next() {
        Some("startpos") => {
            let moves = parse_moves(tokens)?;
            Ok(ParsedPosition {
                startpos: true,
                sfen: None,
                moves,
            })
        }
        Some("sfen") => {
            let mut sfen_tokens = Vec::new();
            while let Some(token) = tokens.peek() {
                if *token == "moves" {
                    break;
                }
                sfen_tokens.push(tokens.next().unwrap().to_string());
            }
            if sfen_tokens.is_empty() {
                bail!("missing SFEN payload");
            }
            let moves = parse_moves(tokens)?;
            Ok(ParsedPosition {
                startpos: false,
                sfen: Some(sfen_tokens.join(" ")),
                moves,
            })
        }
        other => bail!("expected 'startpos' or 'sfen' after 'position', got {:?}", other),
    }
}

/// sfen 文字列だけが渡されたときの簡易パーサ。
pub fn parse_sfen_only(line: &str) -> Result<ParsedPosition> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        bail!("empty SFEN");
    }
    Ok(ParsedPosition {
        startpos: false,
        sfen: Some(trimmed.to_string()),
        moves: Vec::new(),
    })
}

/// moves トークン以降を USI 形式の指し手列として回収する。
pub fn parse_moves<'a, I>(iter: I) -> Result<Vec<String>>
where
    I: Iterator<Item = &'a str>,
{
    let mut iter = iter.peekable();
    match iter.peek() {
        Some(&"moves") => {
            iter.next();
            Ok(iter.map(|mv| mv.to_string()).collect())
        }
        Some(other) => bail!("expected 'moves' before move list, got '{other}'"),
        None => Ok(Vec::new()),
    }
}

pub fn build_position(
    parsed: &ParsedPosition,
    pass_rights_black: Option<u8>,
    pass_rights_white: Option<u8>,
) -> Result<Position> {
    let mut pos = Position::new();
    if parsed.startpos {
        pos.set_sfen(SFEN_HIRATE)?;
    } else if let Some(sfen) = &parsed.sfen {
        pos.set_sfen(sfen)?;
    } else {
        bail!("missing sfen payload");
    }
    // パス権利を有効化（先手または後手の少なくとも一方が指定されている場合）
    if pass_rights_black.is_some() || pass_rights_white.is_some() {
        let black = pass_rights_black.unwrap_or(0);
        let white = pass_rights_white.unwrap_or(0);
        pos.enable_pass_rights(black, white);
    }
    for mv_str in &parsed.moves {
        let mv = Move::from_usi(mv_str)
            .ok_or_else(|| anyhow!("invalid move in start position: {mv_str}"))?;
        if !is_legal_with_pass(&pos, mv) {
            bail!("illegal move '{mv_str}' in start position");
        }
        let gives_check = if mv.is_pass() {
            false
        } else {
            pos.gives_check(mv)
        };
        pos.do_move(mv, gives_check);
    }
    Ok(pos)
}

pub fn describe_position(parsed: &ParsedPosition) -> String {
    let mut buf = OsString::from("position ");
    if parsed.startpos {
        buf.push("startpos");
    } else if let Some(sfen) = &parsed.sfen {
        buf.push("sfen ");
        buf.push(sfen);
    }
    if !parsed.moves.is_empty() {
        buf.push(" moves ");
        buf.push(parsed.moves.join(" "));
    }
    buf.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_position_line_covers_startpos_and_sfen() {
        let parsed = parse_position_line("position startpos moves 7g7f 3c3d").unwrap();
        assert!(parsed.startpos);
        assert_eq!(parsed.moves, vec!["7g7f", "3c3d"]);

        let sfen_line = "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 moves 7g7f";
        let parsed_sfen = parse_position_line(sfen_line).unwrap();
        assert!(!parsed_sfen.startpos);
        assert_eq!(parsed_sfen.moves, vec!["7g7f"]);
        assert!(parsed_sfen.sfen.as_deref().is_some_and(|s| s.starts_with("lnsgkgsnl")));

        let parsed_sfen_only =
            parse_sfen_only("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
                .unwrap();
        assert!(parsed_sfen_only.sfen.is_some());
        assert!(parsed_sfen_only.moves.is_empty());
    }

    #[test]
    fn parse_position_line_rejects_missing_moves_keyword() {
        assert!(parse_position_line("position startpos 7g7f").is_err());
    }
}
