//! 頻度の高い相手の応手を含めた、定跡の継続手数。
use crate::book_db::{apply_usi_move, parse_move_line, position_from_sfen, strip_ply};
use anyhow::{Context, Result};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Args, Debug)]
pub(super) struct DeepArgs {
    /// 継続手数の判定に使う相手の頻度定跡 (YaneuraOu 形式 .db)
    #[arg(long)]
    pub deep_opp_freq: Option<PathBuf>,
    /// 継続手数で辿る相手の手の局数の下限
    #[arg(long, default_value_t = 3, requires = "deep_opp_freq")]
    pub deep_min_games: u64,
}

#[derive(Default)]
pub(super) struct ReplyFrequency {
    moves: BTreeMap<String, BTreeSet<String>>,
    // 下限未満の手しか無い直接キーの局面でも反転キーへ落ちないための印。反転キーが `moves` にある
    // 局面だけを持つので、大きさは残した手の数に比例し、頻度定跡の局面数にはよらない。
    shadowed: BTreeSet<String>,
}

impl ReplyFrequency {
    /// 下限以上の応手を持つ局面キーと指し手だけを読み込む (ファイルを 2 回読む)。
    pub fn read(path: &Path, min_games: u64) -> Result<Self> {
        let open = || {
            File::open(path)
                .map(BufReader::new)
                .with_context(|| format!("頻度定跡を開けません: {}", path.display()))
        };
        Self::from_readers(open, min_games)
            .with_context(|| format!("頻度定跡を読めません: {}", path.display()))
    }

    #[cfg(test)]
    pub(super) fn from_reader(text: &[u8], min_games: u64) -> Result<Self> {
        Self::from_readers(|| Ok(text), min_games)
    }

    /// 1 回目で下限以上の手を、2 回目でその反転キーにあたる直接キーの局面を集める。
    fn from_readers<R: BufRead>(
        mut open: impl FnMut() -> Result<R>,
        min_games: u64,
    ) -> Result<Self> {
        let mut result = Self::default();
        for_each_line(open()?, |key, line| {
            if let Some(m) = parse_move_line(line)
                && m.count >= min_games
                && let Some(mv) = m.move_usi
            {
                result.moves.entry(key.to_owned()).or_default().insert(mv);
            }
        })?;
        // 手の行の無い sfen 行も直接キーとして見つかった扱いにする (`BookDb::find` と同じ)。
        for line in open()?.lines() {
            let line = line?;
            if let Some(sfen) = line.trim_end_matches(['\r', '\n']).strip_prefix("sfen ") {
                let key = strip_ply(sfen.trim());
                if !result.moves.contains_key(key)
                    && rshogi_book::flipped_key(key)
                        .is_some_and(|flipped| result.moves.contains_key(strip_ply(&flipped)))
                {
                    result.shadowed.insert(key.to_owned());
                }
            }
        }
        Ok(result)
    }

    fn replies(&self, key: &str) -> BTreeSet<String> {
        if let Some(moves) = self.moves.get(key) {
            return moves.clone();
        }
        if self.shadowed.contains(key) {
            return BTreeSet::new();
        }
        rshogi_book::flipped_key(key)
            .and_then(|flipped| self.moves.get(strip_ply(&flipped)))
            .into_iter()
            .flatten()
            .filter_map(|mv| rshogi_book::flip_usi_move(mv))
            .collect()
    }
}

/// 局面の手の行ごとに `(局面キー, 行)` を渡す。手の行の無い局面は渡さない。
fn for_each_line(reader: impl BufRead, mut f: impl FnMut(&str, &str)) -> Result<()> {
    let mut key = None;
    for line in reader.lines() {
        let line = line?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(sfen) = line.strip_prefix("sfen ") {
            key = Some(strip_ply(sfen.trim()).to_owned());
        } else if let Some(key) = &key {
            f(key, line);
        }
    }
    Ok(())
}

pub(super) struct Continuation<'a, F> {
    pub freq: &'a ReplyFrequency,
    pub main: F,
}

struct Visited {
    key: String,
    children: Vec<Rc<Visited>>,
}

impl Visited {
    fn is_disjoint(&self, path: &BTreeSet<String>) -> bool {
        let mut pending = vec![self];
        let mut seen = BTreeSet::new();
        while let Some(node) = pending.pop() {
            if !seen.insert(std::ptr::from_ref(node)) {
                continue;
            }
            if path.contains(&node.key) {
                return false;
            }
            pending.extend(node.children.iter().map(Rc::as_ref));
        }
        true
    }
}

#[derive(Clone)]
struct Cached {
    depth: usize,
    visited: Option<Rc<Visited>>,
}

impl<F: Fn(&str) -> Option<String>> Continuation<'_, F> {
    /// 根の手を除き、各分岐で辿れる継続手数の最小値を上限まで返す。
    pub fn after(&self, key: &str, mv: &str, limit: usize) -> Result<usize> {
        let mut pos = position_from_sfen(&format!("{key} 1"))?;
        apply_usi_move(&mut pos, mv)?;
        let own = key.split_whitespace().nth(1) == Some("w");
        let mut path = BTreeSet::from([key.to_owned()]);
        let mut memo = BTreeMap::new();
        Ok(self.depth(strip_ply(&pos.to_sfen()), own, limit, &mut path, &mut memo)?.depth)
    }

    fn depth(
        &self,
        key: &str,
        own: bool,
        remaining: usize,
        path: &mut BTreeSet<String>,
        memo: &mut BTreeMap<(String, usize), Cached>,
    ) -> Result<Cached> {
        let mut result = Cached {
            depth: 0,
            visited: None,
        };
        if remaining == 0 {
            return Ok(result);
        }
        let mut visited = Visited {
            key: key.to_owned(),
            children: Vec::new(),
        };
        if path.contains(key) {
            result.visited = Some(Rc::new(visited));
            return Ok(result);
        }
        let cache_key = (key.to_owned(), remaining);
        // 継続手数は祖先との循環に依存するため、探索した局面が現在の経路と重なるメモは使わない。
        if let Some(cached) = memo.get(&cache_key)
            && cached.visited.as_ref().is_none_or(|v| v.is_disjoint(path))
        {
            return Ok(cached.clone());
        }
        let mut moves = BTreeSet::new();
        let main = (self.main)(key);
        if let Some(main) = &main {
            moves.insert(main.clone());
        }
        if (key.split_whitespace().nth(1) == Some("w")) != own {
            moves.extend(self.freq.replies(key));
        }
        path.insert(key.to_owned());
        let pos = position_from_sfen(&format!("{key} 1"))?;
        let mut shortest = remaining;
        let mut legal_moves = 0;
        for mv in &moves {
            let mut child = pos.clone();
            if apply_usi_move(&mut child, mv).is_err() {
                if main.as_ref() == Some(mv) {
                    shortest = 0;
                    break;
                }
                continue;
            }
            legal_moves += 1;
            let next = self.depth(strip_ply(&child.to_sfen()), own, remaining - 1, path, memo)?;
            shortest = shortest.min(1 + next.depth);
            visited.children.extend(next.visited);
            if shortest == 1 {
                break;
            }
        }
        path.remove(key);
        result.depth = if legal_moves == 0 { 0 } else { shortest };
        result.visited = Some(Rc::new(visited));
        // 祖先への循環で打ち切った値は、別の経路では短すぎるので保存しない。
        if result.visited.as_ref().is_none_or(|v| v.is_disjoint(path)) {
            memo.insert(cache_key, result.clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn frequency_filters_moves_during_loading() {
        let freq = ReplyFrequency::from_reader(
            b"#YANEURAOU-DB2016\nsfen low b - 1\n7g7f none 9 8 2\nsfen kept w - 42\n3c3d none 9 8 3\n8c8d none 9 8 1\nsfen kept w - 7\n4c4d none 9 8 4\nnone none 0 0 99\n".as_slice(), 3,
        ).unwrap();
        assert_eq!(
            freq.moves,
            BTreeMap::from([(
                "kept w -".to_owned(),
                BTreeSet::from(["3c3d".to_owned(), "4c4d".to_owned()]),
            )])
        );
    }

    #[test]
    fn direct_key_with_only_rare_moves_does_not_fall_back_to_flipped_key() {
        let direct = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let flipped = rshogi_book::flipped_key(&format!("{direct} 1")).unwrap();
        let text = format!(
            "#YANEURAOU-DB2016\nsfen {direct} 1\n5i5h none 0 0 2\nsfen {flipped}\n5a5b none 0 0 9\n"
        );
        let freq = ReplyFrequency::from_reader(text.as_bytes(), 3).unwrap();
        assert!(freq.replies(direct).is_empty());
        let only_flipped = ReplyFrequency::from_reader(
            format!("#YANEURAOU-DB2016\nsfen {flipped}\n5a5b none 0 0 9\n").as_bytes(),
            3,
        )
        .unwrap();
        assert_eq!(only_flipped.replies(direct).len(), 1);
    }

    #[test]
    fn direct_key_without_move_lines_does_not_fall_back_in_either_order() {
        let direct = "4k4/9/9/9/9/9/9/9/4K4 w -";
        let flipped = rshogi_book::flipped_key(&format!("{direct} 1")).unwrap();
        let empty = format!("sfen {direct} 1\n");
        let other = format!("sfen {flipped}\n5i6h none 0 0 3\n");
        for text in [format!("{empty}{other}"), format!("{other}{empty}")] {
            let freq = ReplyFrequency::from_reader(text.as_bytes(), 3).unwrap();
            let db = crate::book_db::BookDb::from_reader(text.as_bytes()).unwrap();
            let hit = db.find(&format!("{direct} 1")).unwrap();
            assert!(!hit.flipped && db.entries[&hit.key].moves.is_empty());
            assert!(freq.replies(direct).is_empty());
        }
    }

    #[test]
    fn frequency_bridges_missing_opponent_positions_for_both_sides() {
        let root = "4k4/9/9/9/9/9/9/9/4K4 b -";
        for white in [false, true] {
            let transform_key = |key: &str| {
                if white {
                    strip_ply(&rshogi_book::flipped_key(key).unwrap()).to_owned()
                } else {
                    key.to_owned()
                }
            };
            let transform_move = |mv: &str| {
                if white {
                    rshogi_book::flip_usi_move(mv).unwrap()
                } else {
                    mv.to_owned()
                }
            };
            let root = transform_key(root);
            let root_move = transform_move("5i5h");
            let mut pos = position_from_sfen(&format!("{root} 1")).unwrap();
            apply_usi_move(&mut pos, &root_move).unwrap();
            let opponent = strip_ply(&pos.to_sfen()).to_owned();
            let mut mains = BTreeMap::new();
            for reply in ["5a4b", "5a6b"] {
                let mut child = pos.clone();
                apply_usi_move(&mut child, &transform_move(reply)).unwrap();
                mains.insert(strip_ply(&child.to_sfen()).to_owned(), transform_move("5h5g"));
            }
            let text = format!(
                "sfen {opponent} 2\n{} none 0 0 3\n{} none 0 0 5\n1a1b none 0 0 99\n",
                transform_move("5a4b"),
                transform_move("5a6b")
            );
            let freq = ReplyFrequency::from_reader(text.as_bytes(), 3).unwrap();
            let continuation = Continuation {
                freq: &freq,
                main: |key: &str| mains.get(key).cloned(),
            };
            assert!(!mains.contains_key(&opponent));
            assert_eq!(continuation.after(&root, &root_move, 6).unwrap(), 2);
            mains.pop_first();
            let continuation = Continuation {
                freq: &freq,
                main: |key: &str| mains.get(key).cloned(),
            };
            assert_eq!(continuation.after(&root, &root_move, 6).unwrap(), 1);
            assert!(continuation.after(&root, "1a1b", 0).is_err());
        }
    }

    #[test]
    fn deep_min_games_requires_frequency_in_both_commands() {
        use clap::Parser;
        #[derive(Parser)]
        struct AuditCli {
            #[command(flatten)]
            args: super::super::audit::AuditArgs,
        }
        #[derive(Parser)]
        struct HoldCli {
            #[command(flatten)]
            args: super::super::hold_early::HoldEarlyArgs,
        }
        for with_freq in [false, true] {
            let mut audit = vec![
                "audit",
                "--master",
                "moves.tsv",
                "--table",
                "audit.tsv",
                "--deep-plies",
                "2",
            ];
            let mut hold = vec![
                "hold",
                "--master",
                "moves.tsv",
                "--before",
                "before.tsv",
                "--since",
                "確認",
                "--out",
                "hold.tsv",
            ];
            assert!(AuditCli::try_parse_from(&audit).is_ok());
            assert!(HoldCli::try_parse_from(&hold).is_ok());
            if with_freq {
                audit.extend(["--deep-opp-freq", "freq.db"]);
                hold.extend(["--deep-opp-freq", "freq.db"]);
            }
            audit.extend(["--deep-min-games", "5"]);
            hold.extend(["--deep-min-games", "5"]);
            assert_eq!(AuditCli::try_parse_from(audit).is_ok(), with_freq);
            assert_eq!(HoldCli::try_parse_from(hold).is_ok(), with_freq);
        }
    }

    #[test]
    fn memo_does_not_reuse_a_continuation_stopped_by_an_external_ancestor() {
        let a = "4k4/9/9/9/9/9/9/5K3/9 w -";
        let mut pos = position_from_sfen(&format!("{a} 1")).unwrap();
        apply_usi_move(&mut pos, "5a5b").unwrap();
        let b = strip_ply(&pos.to_sfen()).to_owned();
        let mains = BTreeMap::from([(a.to_owned(), "5a5b"), (b.clone(), "4h4g")]);
        let calls = Cell::new(0);
        let freq = ReplyFrequency::default();
        let continuation = Continuation {
            freq: &freq,
            main: |key: &str| {
                calls.set(calls.get() + 1);
                mains.get(key).map(|m| (*m).to_owned())
            },
        };
        let mut memo = BTreeMap::new();
        let mut path = BTreeSet::from([b]);
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 1);
        path.clear();
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 2);
        let before = calls.get();
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 2);
        assert_eq!(calls.get(), before);
        let first = memo.get(&(a.to_owned(), 3)).unwrap().clone();
        let reused = continuation.depth(a, false, 3, &mut path, &mut memo).unwrap();
        assert!(Rc::ptr_eq(first.visited.as_ref().unwrap(), reused.visited.as_ref().unwrap()));
        path.insert(strip_ply(&pos.to_sfen()).to_owned());
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 1);
        assert!(calls.get() > before);
    }
}
