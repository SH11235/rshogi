//! 局面ごとの定跡乱択候補と警告済み状態。

use crate::{flip, reader::normalize_key};
use rshogi_core::{position::Position, types::Move};
use std::collections::{HashMap, HashSet};

/// 手数を無視した局面キーから、ファイル記載順の乱択候補を引くリスト。
#[derive(Debug, Default)]
pub struct BookExploreList {
    entries: HashMap<String, Vec<String>>,
    warned: HashSet<(String, String)>,
}

impl BookExploreList {
    /// UTF-8 テキストを解析する。不正行は行番号付きで通知して読み飛ばす。
    /// 同一局面の重複行は後勝ち、同一行の重複手は最初の一つを残す。
    pub fn parse(text: &str, mut info: impl FnMut(&str)) -> Self {
        let mut list = Self::default();
        for (line_no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match parse_entry(line) {
                Ok((key, moves)) => {
                    list.entries.insert(key, moves);
                }
                Err(reason) => {
                    info(&format!("BookExploreFile line {}: {reason}; ignored", line_no + 1))
                }
            }
        }
        info(&format!("book explore loaded: {} entries", list.entries.len()));
        list
    }

    /// 登録局面数を返す。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 登録局面がないかを返す。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn moves(&self, sfen: &str, flipped: bool) -> Option<Vec<String>> {
        if let Some(moves) = self.entries.get(&normalize_key(sfen, true)) {
            return Some(moves.clone());
        }
        if !flipped {
            return None;
        }
        let key = normalize_key(&flip::flipped_key(sfen)?, true);
        self.entries.get(&key)?.iter().map(|m| flip::flip_usi_move(m)).collect()
    }

    pub(crate) fn warn_skipped(&mut self, sfen: &str, mv: &str, info: &mut dyn FnMut(&str)) {
        let key = normalize_key(sfen, true);
        if self.warned.insert((key.clone(), mv.to_string())) {
            info(&format!("book explore: skipped {mv} at {key} (illegal or not in book)"));
        }
    }
}

fn parse_entry(line: &str) -> Result<(String, Vec<String>), String> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 5 || fields[0] != "sfen" {
        return Err("expected sfen <board> <turn> <hands> [<ply>] <move> ...".into());
    }
    let has_ply = fields[4].parse::<i32>().is_ok();
    let moves_start = if has_ply { 5 } else { 4 };
    if fields.len() == moves_start {
        return Err("missing moves".into());
    }
    let sfen = fields[1..moves_start].join(" ");
    Position::new().set_sfen(&sfen).map_err(|e| format!("invalid SFEN: {e}"))?;
    let mut moves = Vec::new();
    for token in &fields[moves_start..] {
        if !token.is_ascii()
            || !Move::from_usi_strict(token).is_some_and(|m| m != Move::NONE && m != Move::PASS)
        {
            return Err(format!("invalid USI move: {token}"));
        }
        if !moves.iter().any(|m| m == token) {
            moves.push(token.to_string());
        }
    }
    Ok((normalize_key(&fields[1..4].join(" "), true), moves))
}
