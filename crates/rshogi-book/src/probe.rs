//! 定跡 probe(root 局面 1 回、探索の外で実行)。
//!
//! 選択パイプラインは次の順序で実行する:
//!
//! 1. `USI_OwnBook=false` なら不使用
//! 2. `game_ply > BookMoves` なら不使用
//! 3. find(ply 込みキー / IgnoreBookPly)。miss かつ FlippedBook なら反転局面で再検索し指し手反転。
//!    定跡が複数ある(優先定跡)ときは先頭から順にファイルごとにこれを行い、指し手が 1 つ以上ある
//!    最初の局面を使う
//! 4. `to_move` + pseudo-legal + legal で合法性検証、非合法は info string 警告して除去
//!    explore 登録局面はここで合法な定跡手との積集合から等確率で選び、ponder 解決へ進む。
//! 5. `BookDepthLimit`(0 で無効): 筆頭手 depth 不足なら局面ごと不採用
//! 6. `BookEvalDiff` / `BookEvalBlackLimit` / `BookEvalWhiteLimit`: 下限未満を除去
//!    (5・6 の depth 下限と評価値差は、root 局面の手番側の `BookDepthBlackLimit` / `BookEvalBlackDiff`
//!    等があればそちらを使う)
//! 7. `BookSelectValue`: true なら value 最大手を決定的に選ぶ(同値は count 降順 → USI 昇順)
//! 8. `NarrowBook`: count 情報がある場合のみ出現率 10% 未満を除去
//! 9. 選択: `ConsiderBookMoveCount` なら count 比例抽選(全 0 は等確率)、false は等確率
//! 10. ponder 補完: book の ponder が none なら 1 手進めて再 find し筆頭候補を採用
//! 11. bestmove(+ ponder)を返す

use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};

use std::borrow::Cow;

use crate::reader::{Book, PositionEntry};
use crate::{BookExploreList, flip};

/// 定跡選択に用いる乱数源。テストで固定できるよう抽象化する。
pub trait BookRng {
    /// `[0, n)` の一様乱数を返す。`n == 0` の場合は 0 を返す。
    fn rand_below(&mut self, n: u64) -> u64;
}

/// SplitMix64 ベースの既定乱数源(外部乱数 crate 非依存)。
///
/// 定跡手の抽選用途にのみ使うため、統計的品質は SplitMix64 で十分。
#[derive(Debug, Clone)]
pub struct DefaultBookRng {
    state: u64,
}

impl DefaultBookRng {
    /// システム時刻から種を採って生成する。
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::from_seed(seed)
    }

    /// 種を明示して生成する(再現性が必要な場合・wasm など)。
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Default for DefaultBookRng {
    fn default() -> Self {
        Self::new()
    }
}

impl BookRng for DefaultBookRng {
    fn rand_below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        self.next_u64() % n
    }
}

/// 定跡 probe のオプション群(USI オプションのミラー)。
///
/// 既定値はモジュール doc の選択パイプラインが前提とする値に揃えている。
#[derive(Debug, Clone)]
pub struct BookOptions {
    /// `USI_OwnBook`: 定跡使用の総合スイッチ。
    pub own_book: bool,
    /// `BookMoves`: この手数までしか定跡を使わない。
    pub book_moves: i32,
    /// `BookEvalDiff`: 筆頭手評価値からの許容差。
    pub eval_diff: i32,
    /// `BookEvalBlackLimit`: 先手番の評価値下限。
    pub eval_black_limit: i32,
    /// `BookEvalWhiteLimit`: 後手番の評価値下限。
    pub eval_white_limit: i32,
    /// `BookDepthLimit`: 筆頭手の必要 depth 下限(0 で無効)。
    pub depth_limit: i32,
    /// `NarrowBook`: 出現率 10% 未満の手を除外するか。
    pub narrow_book: bool,
    /// `BookSelectValue`: value 最大手を決定的に選ぶか。
    pub select_value: bool,
    /// `ConsiderBookMoveCount`: 採択回数比例で抽選するか。
    pub consider_move_count: bool,
    /// `FlippedBook`: miss 時に先後反転局面で再検索するか。
    pub flipped_book: bool,
    /// `BookEvalBlackDiff`: 先手番の `eval_diff`。`None` なら `eval_diff` を使う。
    pub eval_black_diff: Option<i32>,
    /// `BookEvalWhiteDiff`: 後手番の `eval_diff`。`None` なら `eval_diff` を使う。
    pub eval_white_diff: Option<i32>,
    /// `BookDepthBlackLimit`: 先手番の `depth_limit`。`None` なら `depth_limit` を使う。
    pub depth_black_limit: Option<i32>,
    /// `BookDepthWhiteLimit`: 後手番の `depth_limit`。`None` なら `depth_limit` を使う。
    pub depth_white_limit: Option<i32>,
}

/// root 局面の手番で決まる評価値差・depth 下限と、info string に出すオプション名。
struct SideLimits {
    eval_diff_name: &'static str,
    eval_diff: i32,
    depth_limit_name: &'static str,
    depth_limit: i32,
}

impl BookOptions {
    /// 手番側の個別値があればそれを、無ければ共通値を使う。評価値差と depth は独立に決める。
    fn side_limits(&self, side: Color) -> SideLimits {
        let (eval_side, depth_side) = match side {
            Color::Black => (
                (self.eval_black_diff, "BookEvalBlackDiff"),
                (self.depth_black_limit, "BookDepthBlackLimit"),
            ),
            Color::White => (
                (self.eval_white_diff, "BookEvalWhiteDiff"),
                (self.depth_white_limit, "BookDepthWhiteLimit"),
            ),
        };
        let (eval_diff, eval_diff_name) = match eval_side {
            (Some(v), name) => (v, name),
            (None, _) => (self.eval_diff, "BookEvalDiff"),
        };
        let (depth_limit, depth_limit_name) = match depth_side {
            (Some(v), name) => (v, name),
            (None, _) => (self.depth_limit, "BookDepthLimit"),
        };
        SideLimits {
            eval_diff_name,
            eval_diff,
            depth_limit_name,
            depth_limit,
        }
    }
}

impl Default for BookOptions {
    fn default() -> Self {
        Self {
            own_book: true,
            book_moves: 16,
            eval_diff: 30,
            eval_black_limit: 0,
            eval_white_limit: -140,
            depth_limit: 0,
            narrow_book: false,
            select_value: false,
            consider_move_count: false,
            flipped_book: true,
            eval_black_diff: None,
            eval_white_diff: None,
            depth_black_limit: None,
            depth_white_limit: None,
        }
    }
}

/// probe 結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookProbeResult {
    /// 採用する指し手(合法性検証済み、32bit 化済み)。
    pub best_move: Move,
    /// 予想手(あれば)。
    pub ponder_move: Option<Move>,
}

/// probe 内部で扱う合法性検証済みの候補手。
struct Candidate {
    /// この局面の座標系での合法手(32bit 化済み)。
    mv: Move,
    /// この局面の座標系での USI 文字列。
    move_usi: String,
    /// この局面の座標系での ponder(USI 文字列)。book に none なら `None`。
    ponder_usi: Option<String>,
    value: i32,
    depth: i32,
    move_count: u64,
}

/// 定跡を先頭から順に find(ply 込み) し、miss なら同じファイルで flip 再検索する (選択パイプラインの 3)。
/// 指し手が 0 個の局面は載っていないものとして次のファイルへ進む。
/// 戻り値の `bool` は反転局面で当たったか。
///
/// 読み出しエラーは miss と区別し、反転局面の再検索にも後ろのファイルにも進まない
/// (丸読みなら当たる局面で、反転側や優先度の低いファイルの手を選ばないため)。
fn find_in_books<'a>(
    books: &'a [Book],
    sfen: &str,
    flipped_book: bool,
    info: &mut dyn FnMut(&str, &str),
) -> Option<(Cow<'a, PositionEntry>, bool)> {
    for book in books {
        let mut find = |key: &str| {
            book.find_raw(key)
                .map_err(|e| info("", &format!("Error reading book file: {e}")))
                .ok()
        };
        let found = match find(sfen)? {
            Some(entry) => Some((entry, false)),
            None if flipped_book => find(&flip::flipped_key(sfen)?)?.map(|entry| (entry, true)),
            None => None,
        };
        if let Some(found) = found.filter(|(entry, _)| !entry.moves.is_empty()) {
            return Some(found);
        }
    }
    None
}

/// find + flip + 合法性検証 + 並び替えまでを行い、筆頭手が先頭に来た候補列を返す。
///
/// 並びは move_count 降順 → value 降順(安定ソート)。
fn find_candidates(
    books: &[Book],
    position: &Position,
    flipped_book: bool,
    info: &mut dyn FnMut(&str, &str),
) -> Option<Vec<Candidate>> {
    let sfen = position.to_sfen();

    let (entry, flipped) = find_in_books(books, &sfen, flipped_book, info)?;

    // 4. 合法性検証。
    let mut candidates: Vec<Candidate> = Vec::with_capacity(entry.moves.len());
    for raw in &entry.moves {
        let Some(move_usi) = &raw.move_usi else {
            continue; // none/resign は指し手なし。
        };
        // flip ヒット時は指し手を元局面の座標系へ戻す。
        let move_str = if flipped {
            match flip::flip_usi_move(move_usi) {
                Some(s) => s,
                None => {
                    info(
                        move_usi,
                        &format!("Illegal Move In Book DB (unparsable flipped move): {move_usi}"),
                    );
                    continue;
                }
            }
        } else {
            move_usi.clone()
        };

        let Some(decoded) = Move::from_usi(&move_str) else {
            info(&move_str, &format!("Illegal Move In Book DB (unparsable move): {move_str}"));
            continue;
        };
        // to_move で 16bit → 32bit 化 + 手番/符号化検証。さらに pseudo-legal + legal で
        // 完全合法性を確認する(探索の movegen を経ずに bestmove として直接返すため)。
        let Some(mv) = position.to_move(decoded) else {
            info(&move_str, &format!("Illegal Move In Book DB: {move_str}"));
            continue;
        };
        if mv == Move::NONE || !position.pseudo_legal(mv) || !position.is_legal(mv) {
            info(&move_str, &format!("Illegal Move In Book DB: {move_str}"));
            continue;
        }

        let ponder_usi = raw.ponder_usi.as_ref().and_then(|p| {
            if flipped {
                flip::flip_usi_move(p)
            } else {
                Some(p.clone())
            }
        });

        candidates.push(Candidate {
            mv,
            move_usi: move_str,
            ponder_usi,
            value: raw.value,
            depth: raw.depth,
            move_count: raw.move_count,
        });
    }

    // move_count 降順 → value 降順(安定ソートでファイル内順序を保つ)。
    candidates.sort_by(|a, b| b.move_count.cmp(&a.move_count).then_with(|| b.value.cmp(&a.value)));

    Some(candidates)
}

/// 候補列から採用手の添字を選ぶ。`consider_move_count=false` は等確率、
/// `true` は採択回数比例の 1 パス・オンライン重み付き抽選。
fn select_index(
    candidates: &[Candidate],
    consider_move_count: bool,
    rng: &mut dyn BookRng,
) -> usize {
    let n = candidates.len();
    if !consider_move_count {
        return rng.rand_below(n as u64) as usize;
    }

    // 候補は count 降順ソート済みなので先頭の weight は必ず 1 以上
    // (全手 count=0 のときは各手を 1 とみなす等確率フォールバック)。
    // よって cumulative は常に正で、i=0 で必ず idx=0 が採用される。
    let sum: u64 = candidates.iter().map(|c| c.move_count).sum();
    let mut idx = 0;
    let mut cumulative: u64 = 0;
    for (i, c) in candidates.iter().enumerate() {
        let weight = if sum == 0 { 1 } else { c.move_count };
        cumulative += weight;
        if rng.rand_below(cumulative) < weight {
            idx = i;
        }
    }

    idx
}

/// value 最大手を決定的に選ぶ。同値は count 降順 → USI 昇順。
fn select_value_index(candidates: &[Candidate]) -> usize {
    candidates
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            a.value
                .cmp(&b.value)
                .then_with(|| a.move_count.cmp(&b.move_count))
                .then_with(|| b.move_usi.cmp(&a.move_usi))
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// ponder を解決する。book に ponder があれば検証して採用、無ければ 1 手進めて筆頭手を拾う。
fn resolve_ponder(
    books: &[Book],
    position: &Position,
    options: &BookOptions,
    best_move: Move,
    book_ponder: &Option<String>,
) -> Option<Move> {
    // best_move は find_candidates で完全合法性を検証済み。安全に 1 手進められる。
    let gives_check = position.gives_check(best_move);
    let mut child = position.clone();
    child.do_move(best_move, gives_check);

    if let Some(ponder_usi) = book_ponder {
        // book 由来の ponder を子局面で検証。合法なら採用、非合法なら ponder なし。
        let decoded = Move::from_usi(ponder_usi)?;
        let mv = child.to_move(decoded)?;
        if mv != Move::NONE && child.pseudo_legal(mv) && child.is_legal(mv) {
            return Some(mv);
        }
        return None;
    }

    // ponder 補完: 子局面を再 find し筆頭候補を ponder に。
    let child_candidates = find_candidates(books, &child, options.flipped_book, &mut |_, _| {})?;
    child_candidates.first().map(|c| c.mv)
}

/// root 局面に対して定跡を probe する。ヒットしなければ `None`。
///
/// `books` は優先順 (先頭ほど優先。[`priority_book_paths`](crate::priority_book_paths) の順)。
/// 局面が載っている最初の定跡だけを使い、その定跡で候補が全て除外されても後ろの定跡には進まない。
///
/// `info` には除外理由等の info string 本文(`"info string "` プレフィックスなし)が渡される。
pub fn probe(
    books: &[Book],
    position: &Position,
    options: &BookOptions,
    rng: &mut dyn BookRng,
    info: impl FnMut(&str),
) -> Option<BookProbeResult> {
    probe_with_explore(books, position, options, rng, None, info)
}

/// 局面指定の乱択候補を優先して定跡を probe する。
/// `info` は `probe` と同じく info string 本文を受け取る。
pub fn probe_with_explore(
    books: &[Book],
    position: &Position,
    options: &BookOptions,
    rng: &mut dyn BookRng,
    explore: Option<&mut BookExploreList>,
    mut info: impl FnMut(&str),
) -> Option<BookProbeResult> {
    // 1. 総合スイッチ。
    if !options.own_book {
        return None;
    }
    // 2. 手数制限。
    if position.game_ply() > options.book_moves {
        return None;
    }

    // 3-4. find + flip + 合法性検証 + 並び替え。
    let sfen = position.to_sfen();
    let explore_moves = explore.as_ref().and_then(|list| list.moves(&sfen, options.flipped_book));
    let mut candidates =
        find_candidates(books, position, options.flipped_book, &mut |mv, message| {
            // リスト指定手の警告は、下の warn_skipped に集約して重複を防ぐ。
            if !explore_moves.as_ref().is_some_and(|moves| moves.iter().any(|m| m == mv)) {
                info(message);
            }
        })?;

    // explore は合法な定跡手との積集合から等確率で選ぶ。通常の絞り込みより優先。
    if let (Some(explore), Some(moves)) = (explore, explore_moves) {
        let mut eligible = Vec::new();
        for mv in moves {
            if let Some(candidate) = candidates.iter().find(|c| c.move_usi == mv) {
                eligible.push(candidate);
            } else {
                explore.warn_skipped(&sfen, &mv, &mut info);
            }
        }
        if !eligible.is_empty() {
            let chosen = eligible[rng.rand_below(eligible.len() as u64) as usize];
            let names: Vec<_> = eligible.iter().map(|c| c.move_usi.as_str()).collect();
            info(&format!("book explore: {} from [{}]", chosen.move_usi, names.join(", ")));
            return Some(BookProbeResult {
                best_move: chosen.mv,
                ponder_move: resolve_ponder(
                    books,
                    position,
                    options,
                    chosen.mv,
                    &chosen.ponder_usi,
                ),
            });
        }
    }
    if candidates.is_empty() {
        return None;
    }

    // FlippedBook で反転局面を引いても、使う側は元の root 局面の手番で決める。
    let limits = options.side_limits(position.side_to_move());

    // 5. BookDepthLimit(0 で無効): 筆頭手の depth 不足なら局面ごと不採用。
    if limits.depth_limit != 0 && candidates[0].depth < limits.depth_limit {
        info(&format!(
            "Top move depth is below {} = {}.",
            limits.depth_limit_name, limits.depth_limit
        ));
        return None;
    }

    // 6. 評価値フィルタ。
    {
        let top_value = candidates.iter().map(|c| c.value).max().unwrap_or(0);
        let side_limit = if position.side_to_move() == Color::Black {
            options.eval_black_limit
        } else {
            options.eval_white_limit
        };
        let value_limit = (top_value - limits.eval_diff).max(side_limit);
        let before = candidates.len();
        candidates.retain(|c| c.value >= value_limit);
        if candidates.len() != before {
            info(&format!(
                "{} = {} : {} moves to {} moves.",
                limits.eval_diff_name,
                limits.eval_diff,
                before,
                candidates.len()
            ));
        }
        if candidates.is_empty() {
            return None;
        }
    }

    // 7. BookSelectValue: 評価値フィルタ後の生存候補から value 最大手を決定的に選ぶ。
    if options.select_value {
        let idx = select_value_index(&candidates);
        let chosen = &candidates[idx];
        let best_move = chosen.mv;
        let book_ponder = chosen.ponder_usi.clone();
        let ponder_move = resolve_ponder(books, position, options, best_move, &book_ponder);

        return Some(BookProbeResult {
            best_move,
            ponder_move,
        });
    }

    // 8. NarrowBook: count 情報がある場合のみ出現率 10% 未満を除去。
    if options.narrow_book {
        let total: u64 = candidates.iter().map(|c| c.move_count).sum();
        if total > 0 {
            let before = candidates.len();
            let threshold = total as f64 * 0.1;
            candidates.retain(|c| c.move_count as f64 >= threshold);
            if candidates.len() != before {
                info(&format!("NarrowBook : {} moves to {} moves.", before, candidates.len()));
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // 9. 選択。
    let idx = select_index(&candidates, options.consider_move_count, rng);
    let chosen = &candidates[idx];
    let best_move = chosen.mv;
    let book_ponder = chosen.ponder_usi.clone();

    // 10. ponder 解決。
    let ponder_move = resolve_ponder(books, position, options, best_move, &book_ponder);

    Some(BookProbeResult {
        best_move,
        ponder_move,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "#YANEURAOU-DB2016 1.00";
    const HIRATE: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    /// 決定的テスト用の乱数源(与えた列を順に返し、尽きたら 0)。
    struct SeqRng {
        values: Vec<u64>,
        idx: usize,
    }
    impl SeqRng {
        fn new(values: Vec<u64>) -> Self {
            Self { values, idx: 0 }
        }
    }
    impl BookRng for SeqRng {
        fn rand_below(&mut self, n: u64) -> u64 {
            if n == 0 {
                return 0;
            }
            let v = self.values.get(self.idx).copied().unwrap_or(0);
            self.idx += 1;
            v % n
        }
    }

    fn pos(sfen: &str) -> Position {
        let mut p = Position::new();
        p.set_sfen(sfen).unwrap();
        p
    }

    fn no_info(_: &str) {}

    fn explore_book() -> Book {
        Book::from_reader(format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 100 16 100\n2g2f 8c8d 0 1 1\n5e5f none 200 16 10\n").as_bytes(), false).unwrap()
    }

    #[test]
    fn explore_overrides_select_value_and_preserves_ponder() {
        let book = explore_book();
        let options = BookOptions {
            select_value: true,
            eval_diff: 300,
            ..Default::default()
        };
        let normal = probe(
            std::slice::from_ref(&book),
            &pos(HIRATE),
            &options,
            &mut SeqRng::new(vec![1]),
            no_info,
        )
        .unwrap();
        assert_eq!(normal.best_move.to_usi(), "7g7f");
        let mut explore = BookExploreList::parse(&format!("sfen {HIRATE} 7g7f 2g2f 7g7f"), no_info);
        for (index, expected, ponder) in [(0, "7g7f", "3c3d"), (1, "2g2f", "8c8d")] {
            let mut messages = Vec::new();
            let result = probe_with_explore(
                std::slice::from_ref(&book),
                &pos(HIRATE),
                &options,
                &mut SeqRng::new(vec![index]),
                Some(&mut explore),
                |m| messages.push(m.to_string()),
            )
            .unwrap();
            assert_eq!(result.best_move.to_usi(), expected);
            assert_eq!(result.ponder_move.unwrap().to_usi(), ponder);
            assert!(messages.contains(&format!("book explore: {expected} from [7g7f, 2g2f]")));
        }
    }

    #[test]
    fn explore_bypasses_eval_diff_limits_depth_narrow_and_count() {
        let book = explore_book();
        let mut explore = BookExploreList::parse(&format!("sfen {HIRATE} 7g7f 2g2f"), no_info);
        let options = BookOptions {
            eval_diff: 0,
            ..Default::default()
        };
        assert_eq!(
            probe(
                std::slice::from_ref(&book),
                &pos(HIRATE),
                &options,
                &mut SeqRng::new(vec![1]),
                no_info
            )
            .unwrap()
            .best_move
            .to_usi(),
            "7g7f"
        );
        for options in [
            options,
            BookOptions {
                eval_black_limit: 999,
                depth_limit: 999,
                narrow_book: true,
                consider_move_count: true,
                select_value: true,
                ..Default::default()
            },
        ] {
            let result = probe_with_explore(
                std::slice::from_ref(&book),
                &pos(HIRATE),
                &options,
                &mut SeqRng::new(vec![1]),
                Some(&mut explore),
                no_info,
            )
            .unwrap();
            assert_eq!(result.best_move.to_usi(), "2g2f");
        }
    }

    #[test]
    fn explore_skips_invalid_once_and_falls_back() {
        let book = explore_book();
        for moves in ["6g6f 5e5f 2g2f", "6g6f 5e5f"] {
            let mut explore = BookExploreList::parse(&format!("sfen {HIRATE} {moves}"), no_info);
            let mut warnings = Vec::new();
            for _ in 0..2 {
                let result = probe_with_explore(
                    std::slice::from_ref(&book),
                    &pos(HIRATE),
                    &BookOptions::default(),
                    &mut SeqRng::new(vec![0]),
                    Some(&mut explore),
                    |m| warnings.push(m.to_string()),
                )
                .unwrap();
                assert_eq!(
                    result.best_move.to_usi(),
                    if moves.ends_with("2g2f") {
                        "2g2f"
                    } else {
                        "7g7f"
                    }
                );
            }
            assert_eq!(
                warnings
                    .iter()
                    .filter(|m| m.starts_with("book explore: skipped")
                        || m.starts_with("Illegal Move In Book DB"))
                    .count(),
                2
            );
        }
    }

    #[test]
    fn explore_normalizes_noncanonical_hands() {
        let canonical = "4k4/9/9/9/9/9/9/9/4K4 b RP 1";
        let book = Book::from_reader(
            format!("{HEADER}\nsfen {canonical}\nP*5e none 0 16 1\n").as_bytes(),
            false,
        )
        .unwrap();
        let mut explore =
            BookExploreList::parse("sfen 4k4/9/9/9/9/9/9/9/4K4 b PR 99 P*5e", no_info);
        assert_eq!(explore.moves(canonical, false).unwrap(), ["P*5e"]);
        let mut messages = Vec::new();
        let result = probe_with_explore(
            std::slice::from_ref(&book),
            &pos(canonical),
            &BookOptions::default(),
            &mut SeqRng::new(vec![0]),
            Some(&mut explore),
            |m| messages.push(m.to_string()),
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "P*5e");
        assert_eq!(messages, ["book explore: P*5e from [P*5e]"]);
    }

    #[test]
    fn explore_preserves_unlisted_illegal_move_warnings() {
        let book = explore_book();
        for text in ["".to_string(), format!("sfen {HIRATE} 2g2f")] {
            let mut explore = BookExploreList::parse(&text, no_info);
            let mut warnings = Vec::new();
            for _ in 0..2 {
                probe_with_explore(
                    std::slice::from_ref(&book),
                    &pos(HIRATE),
                    &BookOptions::default(),
                    &mut SeqRng::new(vec![0]),
                    Some(&mut explore),
                    |m| warnings.push(m.to_string()),
                )
                .unwrap();
            }
            assert_eq!(
                warnings.iter().filter(|m| *m == "Illegal Move In Book DB: 5e5f").count(),
                2
            );
        }
    }

    #[test]
    fn explore_flipped_key_and_direct_key_priority() {
        let book = explore_book();
        let flipped = flip::flipped_key(HIRATE).unwrap();
        let mut explore = BookExploreList::parse(&format!("sfen {flipped} 8c8d"), no_info);
        for (flipped_book, expected) in [(true, "2g2f"), (false, "7g7f")] {
            let options = BookOptions {
                flipped_book,
                ..Default::default()
            };
            let result = probe_with_explore(
                std::slice::from_ref(&book),
                &pos(HIRATE),
                &options,
                &mut SeqRng::new(vec![0]),
                Some(&mut explore),
                no_info,
            )
            .unwrap();
            assert_eq!(result.best_move.to_usi(), expected);
        }
        let mut explore =
            BookExploreList::parse(&format!("sfen {flipped} 8c8d\nsfen {HIRATE} 7g7f"), no_info);
        let result = probe_with_explore(
            std::slice::from_ref(&book),
            &pos(HIRATE),
            &BookOptions::default(),
            &mut SeqRng::new(vec![0]),
            Some(&mut explore),
            no_info,
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
        // 定跡自体も反転検索した場合、ponder も元局面の座標に戻る。
        let result = probe_with_explore(
            std::slice::from_ref(&book),
            &pos(&flipped),
            &BookOptions::default(),
            &mut SeqRng::new(vec![0]),
            Some(&mut explore),
            no_info,
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "8c8d");
        assert_eq!(result.ponder_move.unwrap().to_usi(), "2g2f");
    }

    #[test]
    fn explore_respects_book_preconditions_and_unlisted_positions() {
        let book = explore_book();
        let mut explore = BookExploreList::parse(&format!("sfen {HIRATE} 2g2f"), no_info);
        for options in [
            BookOptions {
                own_book: false,
                ..Default::default()
            },
            BookOptions {
                book_moves: 0,
                ..Default::default()
            },
        ] {
            assert!(
                probe_with_explore(
                    std::slice::from_ref(&book),
                    &pos(HIRATE),
                    &options,
                    &mut SeqRng::new(vec![0]),
                    Some(&mut explore),
                    no_info
                )
                .is_none()
            );
        }
        let empty_book = Book::from_reader(format!("{HEADER}\n").as_bytes(), false).unwrap();
        assert!(
            probe_with_explore(
                std::slice::from_ref(&empty_book),
                &pos(HIRATE),
                &BookOptions::default(),
                &mut SeqRng::new(vec![0]),
                Some(&mut explore),
                no_info
            )
            .is_none()
        );
        let mut empty = BookExploreList::default();
        assert_eq!(
            probe_with_explore(
                std::slice::from_ref(&book),
                &pos(HIRATE),
                &BookOptions::default(),
                &mut SeqRng::new(vec![1]),
                Some(&mut empty),
                no_info
            )
            .unwrap()
            .best_move
            .to_usi(),
            "7g7f"
        );
    }

    #[test]
    fn explore_parser_comments_optional_ply_and_bad_lines() {
        let key = crate::reader::normalize_key(HIRATE, true);
        let flipped = flip::flipped_key(HIRATE).unwrap();
        let text = format!(
            "# comment\n\n  # indented\nsfen {key} 7g7f\nsfen {flipped} 8c8d\nwrong\nsfen bad b - 7g7f\nsfen {HIRATE}\nsfen {HIRATE} 7g7f garbage\nsfen {HIRATE} éé\n"
        );
        let mut messages = Vec::new();
        let explore = BookExploreList::parse(&text, |m| messages.push(m.to_string()));
        assert_eq!(explore.len(), 2);
        assert_eq!(explore.moves(&format!("{key} 99"), false).unwrap(), ["7g7f"]);
        for line in 6..=10 {
            assert!(
                messages.iter().any(|m| m.starts_with(&format!("BookExploreFile line {line}:")))
            );
        }
        assert_eq!(messages.last().unwrap(), "book explore loaded: 2 entries");
    }

    #[test]
    fn probe_hits_and_returns_bestmove() {
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 30 16 100\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let mut rng = SeqRng::new(vec![0]);
        let result = probe(
            std::slice::from_ref(&book),
            &pos(HIRATE),
            &BookOptions::default(),
            &mut rng,
            no_info,
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
        assert_eq!(result.ponder_move.map(|m| m.to_usi()).as_deref(), Some("3c3d"));
    }

    #[test]
    fn probe_miss_returns_none() {
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 30 16 100\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        // 定跡に無い局面(2 手目 3c3d 後)。flip でもヒットしない。
        let other = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3";
        let mut rng = SeqRng::new(vec![0]);
        assert!(
            probe(
                std::slice::from_ref(&book),
                &pos(other),
                &BookOptions::default(),
                &mut rng,
                no_info
            )
            .is_none()
        );
    }

    #[test]
    fn flipped_book_hits_on_one_sided_db() {
        // 片側正規化定跡: 後手番局面 after_76 を flip した「先手番の正準局面」だけを登録する。
        let after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
        let flipped_sfen = flip::flipped_key(after_76).unwrap(); // 先手番(黒)の局面
        // 黒視点フレームでの指し手 7g7f、ponder 8c8d を登録。
        // probe 時に after_76 の座標系へ flip され、白の 3c3d、ponder は 2g2f になる。
        let data = format!("{HEADER}\nsfen {flipped_sfen}\n7g7f 8c8d 20 16 50\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();

        let mut rng = SeqRng::new(vec![0]);
        let result = probe(
            std::slice::from_ref(&book),
            &pos(after_76),
            &BookOptions::default(),
            &mut rng,
            no_info,
        )
        .unwrap();
        // flip で戻された指し手が after_76(白番)局面の合法手であること。
        assert_eq!(result.best_move.to_usi(), "3c3d");
        assert!(pos(after_76).is_legal(result.best_move));
        // ponder も flip される(黒視点 8c8d → after_76 側では 2g2f)。
        assert_eq!(result.ponder_move.map(|m| m.to_usi()).as_deref(), Some("2g2f"));
    }

    #[test]
    fn depth_limit_rejects_whole_node() {
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 30 5 100\n2g2f 8c8d 25 20 40\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        // 筆頭手(count 降順で 7g7f, depth=5)が depth_limit=16 未満 → 局面ごと不採用。
        let opts = BookOptions {
            depth_limit: 16,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![0]);
        assert!(
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).is_none()
        );
    }

    #[test]
    fn depth_limit_zero_is_disabled() {
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 30 0 100\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            depth_limit: 0,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![0]);
        assert!(
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).is_some()
        );
    }

    const AFTER_7G7F: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";

    /// 先手番・後手番の局面を 1 つずつ持つ定跡。どちらも筆頭手 value=100、2 番手 value=80、
    /// depth は先手番 10・後手番 3。
    fn both_sides_book() -> Book {
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 100 10 10\n2g2f 8c8d 80 10 5\n\
             sfen {AFTER_7G7F}\n3c3d 2g2f 100 3 10\n8c8d 2g2f 80 3 5\n"
        );
        Book::from_reader(data.as_bytes(), false).unwrap()
    }

    /// probe して `(指し手, info 文字列)` を返す。抽選は常に先頭。
    fn probe_info(book: &Book, sfen: &str, opts: &BookOptions) -> (Option<String>, Vec<String>) {
        let mut messages = Vec::new();
        let mut rng = SeqRng::new(vec![0]);
        let result = probe(std::slice::from_ref(book), &pos(sfen), opts, &mut rng, |m| {
            messages.push(m.to_string())
        });
        (result.map(|r| r.best_move.to_usi()), messages)
    }

    fn has(messages: &[String], expected: &str) -> bool {
        messages.iter().any(|m| m == expected)
    }

    fn filtered(messages: &[String]) -> bool {
        messages.iter().any(|m| m.contains("moves to"))
    }

    #[test]
    fn unset_per_side_options_use_shared_values_on_both_sides() {
        let book = both_sides_book();
        let opts = BookOptions {
            eval_diff: 0,
            ..Default::default()
        };
        for sfen in [HIRATE, AFTER_7G7F] {
            let (_, messages) = probe_info(&book, sfen, &opts);
            assert!(has(&messages, "BookEvalDiff = 0 : 2 moves to 1 moves."), "{messages:?}");
        }
        let opts = BookOptions {
            depth_limit: 4,
            ..Default::default()
        };
        assert!(probe_info(&book, HIRATE, &opts).0.is_some());
        let (best, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert_eq!(best, None);
        assert!(has(&messages, "Top move depth is below BookDepthLimit = 4."), "{messages:?}");
    }

    #[test]
    fn per_side_eval_diff_applies_only_to_its_side() {
        let book = both_sides_book();
        let opts = BookOptions {
            eval_diff: 0,
            eval_black_diff: Some(30),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, HIRATE, &opts);
        assert_eq!(best.as_deref(), Some("7g7f"));
        assert!(!filtered(&messages), "{messages:?}");
        let (_, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert!(has(&messages, "BookEvalDiff = 0 : 2 moves to 1 moves."), "{messages:?}");

        let opts = BookOptions {
            eval_diff: 30,
            eval_white_diff: Some(0),
            ..Default::default()
        };
        let (_, messages) = probe_info(&book, HIRATE, &opts);
        assert!(!filtered(&messages), "{messages:?}");
        let (best, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert_eq!(best.as_deref(), Some("3c3d"));
        assert!(has(&messages, "BookEvalWhiteDiff = 0 : 2 moves to 1 moves."), "{messages:?}");
    }

    #[test]
    fn per_side_depth_limit_applies_only_to_its_side_and_independently_of_eval_diff() {
        let book = both_sides_book();
        // 後手番の depth だけ個別。評価値差は共通の 0 のまま。
        let opts = BookOptions {
            eval_diff: 0,
            depth_limit: 0,
            depth_white_limit: Some(4),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert_eq!(best, None);
        assert!(
            has(&messages, "Top move depth is below BookDepthWhiteLimit = 4."),
            "{messages:?}"
        );
        let (best, messages) = probe_info(&book, HIRATE, &opts);
        assert_eq!(best.as_deref(), Some("7g7f"));
        assert!(has(&messages, "BookEvalDiff = 0 : 2 moves to 1 moves."), "{messages:?}");

        let opts = BookOptions {
            depth_limit: 0,
            depth_black_limit: Some(11),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, HIRATE, &opts);
        assert_eq!(best, None);
        assert!(
            has(&messages, "Top move depth is below BookDepthBlackLimit = 11."),
            "{messages:?}"
        );
        assert!(probe_info(&book, AFTER_7G7F, &opts).0.is_some());
    }

    #[test]
    fn per_side_zero_and_max_values_override_shared_values() {
        let book = both_sides_book();
        // 個別の 0 は「継承」ではなく 0 (評価値差 0、depth 下限なし)。
        let opts = BookOptions {
            eval_diff: 30000,
            depth_limit: 999,
            eval_black_diff: Some(0),
            depth_black_limit: Some(0),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, HIRATE, &opts);
        assert_eq!(best.as_deref(), Some("7g7f"));
        assert!(has(&messages, "BookEvalBlackDiff = 0 : 2 moves to 1 moves."), "{messages:?}");

        let opts = BookOptions {
            eval_diff: 0,
            eval_white_diff: Some(99999),
            depth_white_limit: Some(99999),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert_eq!(best, None);
        assert!(
            has(&messages, "Top move depth is below BookDepthWhiteLimit = 99999."),
            "{messages:?}"
        );
        let opts = BookOptions {
            depth_white_limit: None,
            ..opts
        };
        let (_, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert!(!filtered(&messages), "{messages:?}");
    }

    #[test]
    fn per_side_options_use_root_side_even_when_hit_by_flipped_book() {
        // 後手番の root (7g7f の後) を、先後反転した先手番の局面だけで登録した定跡。
        let flipped_sfen = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 2";
        let data = format!("{HEADER}\nsfen {flipped_sfen}\n7g7f 8c8d 100 3 10\n2g2f 8c8d 80 3 5\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            eval_black_diff: Some(30000),
            eval_white_diff: Some(0),
            depth_black_limit: Some(999),
            depth_white_limit: Some(3),
            ..Default::default()
        };
        let (best, messages) = probe_info(&book, AFTER_7G7F, &opts);
        assert_eq!(best.as_deref(), Some("3c3d"));
        assert!(has(&messages, "BookEvalWhiteDiff = 0 : 2 moves to 1 moves."), "{messages:?}");
    }

    #[test]
    fn eval_diff_filters_low_value_moves() {
        // 筆頭手 value=100、2 番手 value=50。eval_diff=30 → 下限 70、50 は除去。
        // count を等しくして value 順で並ぶようにする。
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 100 16 10\n2g2f 8c8d 50 16 10\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            eval_diff: 30,
            eval_black_limit: -30000,
            consider_move_count: false,
            ..Default::default()
        };
        // 候補が 1 手に絞られるので抽選結果は常に 7g7f。
        let mut rng = SeqRng::new(vec![0, 0, 0]);
        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
    }

    #[test]
    fn eval_diff_anchors_on_max_value_not_highest_count_move() {
        // count 降順の筆頭手 value=-52 ではなく、候補中の最大 value=-41 を基準にする。
        // eval_diff=30 なので正しい下限は -71、value=-81 の手は除去される。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d -52 16 100\n2g2f 8c8d -41 16 10\n6g6f 4c4d -81 16 1\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            eval_diff: 30,
            eval_black_limit: -30000,
            consider_move_count: false,
            ..Default::default()
        };

        let old_anchor_limit = (-52_i32 - opts.eval_diff).max(opts.eval_black_limit);
        assert_eq!(old_anchor_limit, -82);
        assert!(-81 >= old_anchor_limit);

        let mut warnings = Vec::new();
        let mut rng = SeqRng::new(vec![2]);
        let result = probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, |m| {
            warnings.push(m.to_string())
        })
        .unwrap();

        assert_ne!(result.best_move.to_usi(), "6g6f");
        assert!(warnings.iter().any(|w| w == "BookEvalDiff = 30 : 3 moves to 2 moves."));
    }

    #[test]
    fn eval_diff_keeps_count_only_zero_value_moves() {
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 0 16 30\n2g2f 8c8d 0 16 20\n6g6f 4c4d 0 16 10\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            eval_diff: 30,
            eval_black_limit: -30000,
            consider_move_count: false,
            ..Default::default()
        };

        let mut warnings = Vec::new();
        let mut rng = SeqRng::new(vec![2]);
        let result = probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, |m| {
            warnings.push(m.to_string())
        })
        .unwrap();

        assert_eq!(result.best_move.to_usi(), "6g6f");
        assert!(!warnings.iter().any(|w| w.starts_with("BookEvalDiff = ")));
    }

    #[test]
    fn eval_black_limit_rejects_when_top_too_low() {
        // 先手番で筆頭手 value=-50 が black_limit=0 を下回る → 全除去。
        let data = format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d -50 16 10\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            eval_black_limit: 0,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![0]);
        assert!(
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).is_none()
        );
    }

    #[test]
    fn narrow_book_removes_rare_moves() {
        // count: 90 / 5 / 5 → 5% の 2 手を除去、残 1 手。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 0 0 90\n2g2f 8c8d 0 0 5\n2h6h none 0 0 5\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            narrow_book: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![0]);
        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
    }

    #[test]
    fn book_select_value_chooses_highest_value_move() {
        // value 最大の 2g2f は count が低く NarrowBook なら除去対象だが、
        // BookSelectValue=true では NarrowBook / count 抽選をスキップして採用する。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 10 16 100\n2g2f 8c8d 80 16 1\n6g6f 4c4d 20 16 50\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            select_value: true,
            narrow_book: true,
            consider_move_count: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![0, 0, 0]);

        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();

        assert_eq!(result.best_move.to_usi(), "2g2f");
    }

    #[test]
    fn book_select_value_tiebreaks_by_count_then_usi() {
        // value 同値なら count 最大、count も同値なら USI 昇順。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 80 16 40\n2g2f 8c8d 80 16 50\n1g1f 9c9d 80 16 50\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            select_value: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        let mut rng = SeqRng::new(vec![2, 2, 2]);

        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();

        assert_eq!(result.best_move.to_usi(), "1g1f");
    }

    #[test]
    fn book_select_value_false_keeps_existing_selection_pipeline() {
        // BookSelectValue=false では value 最大の 2g2f を貪欲選択せず、
        // 既存どおり NarrowBook 後に count 比例抽選する。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 10 16 100\n2g2f 8c8d 80 16 1\n6g6f 4c4d 20 16 50\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            select_value: false,
            narrow_book: true,
            consider_move_count: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        // NarrowBook 後の並びは [7g7f(100), 6g6f(50)]。
        // i=0: 0<100 で idx=0、i=1: 149<50? false なので 7g7f のまま。
        let mut rng = SeqRng::new(vec![0, 149]);

        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();

        assert_eq!(result.best_move.to_usi(), "7g7f");
    }

    #[test]
    fn weighted_selection_respects_counts() {
        // 3 手: count 10 / 20 / 70。ConsiderBookMoveCount=true。
        // SeqRng を制御して 3 番手(70)が選ばれる系列を作る。
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 0 0 70\n2g2f 8c8d 0 0 20\n6g6f 4c4d 0 0 10\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            consider_move_count: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        // 並びは count 降順: [7g7f(70), 2g2f(20), 6g6f(10)]。
        // select: 各手で rand_below(cumulative)。cumulative: 70, 90, 100。
        // 系列 → i=0 で採用(0<70)、i=1 で 89<20?false、i=2 で 99<10?false → 7g7f。
        let mut rng = SeqRng::new(vec![0, 89, 99]);
        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
    }

    #[test]
    fn weighted_all_zero_counts_is_uniform() {
        // 全手 count=0 → 各手 weight=1 の等確率抽選。base の乱数で決まる。
        let data =
            format!("{HEADER}\nsfen {HIRATE}\n7g7f 3c3d 0 0 0\n2g2f 8c8d 0 0 0\n6g6f 4c4d 0 0 0\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let opts = BookOptions {
            consider_move_count: true,
            eval_black_limit: -30000,
            eval_diff: 30000,
            ..Default::default()
        };
        // sum=0 なので weight=1、cumulative=1,2,3。系列で i=1 を選ばせる:
        // i=0: rand_below(1)=0<1 → idx=0; i=1: rand_below(2)=0<1 → idx=1;
        // i=2: rand_below(3)=2<1?false → idx=1。
        let mut rng = SeqRng::new(vec![0, 0, 2]);
        let result =
            probe(std::slice::from_ref(&book), &pos(HIRATE), &opts, &mut rng, no_info).unwrap();
        assert_eq!(result.best_move.to_usi(), "2g2f");
    }

    #[test]
    fn illegal_book_move_is_skipped_with_warning() {
        // 1 手目に非合法手(自陣に無い駒の移動)を混ぜる。合法な 2g2f のみ残る。
        let data = format!("{HEADER}\nsfen {HIRATE}\n5e5f 3c3d 0 0 100\n2g2f 8c8d 0 0 50\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let mut warnings = Vec::new();
        let mut rng = SeqRng::new(vec![0]);
        let result = probe(
            std::slice::from_ref(&book),
            &pos(HIRATE),
            &BookOptions::default(),
            &mut rng,
            |m| warnings.push(m.to_string()),
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "2g2f");
        assert!(warnings.iter().any(|w| w.contains("Illegal Move In Book DB")));
    }

    #[test]
    fn ponder_completion_when_book_ponder_none() {
        // 1 手目 ponder=none。子局面(7g7f 後)を find し筆頭手を ponder に補完。
        let after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
        let data = format!(
            "{HEADER}\nsfen {HIRATE}\n7g7f none 0 0 100\nsfen {after_76}\n3c3d 2g2f 0 0 100\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        let mut rng = SeqRng::new(vec![0]);
        let result = probe(
            std::slice::from_ref(&book),
            &pos(HIRATE),
            &BookOptions::default(),
            &mut rng,
            no_info,
        )
        .unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
        assert_eq!(result.ponder_move.map(|m| m.to_usi()).as_deref(), Some("3c3d"));
    }

    fn db_book(body: &str) -> Book {
        Book::from_reader(format!("{HEADER}\n{body}").as_bytes(), false).unwrap()
    }

    fn best_and_ponder(
        books: &[Book],
        sfen: &str,
        options: &BookOptions,
    ) -> Option<(String, String)> {
        probe(books, &pos(sfen), options, &mut SeqRng::new(vec![0]), no_info)
            .map(|r| (r.best_move.to_usi(), r.ponder_move.map(|m| m.to_usi()).unwrap_or_default()))
    }

    #[test]
    fn priority_books_use_first_book_with_the_position() {
        let other = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3";
        let books = [
            db_book(&format!("sfen {other}\n2g2f none 0 0 1\n")),
            db_book(&format!("sfen {HIRATE}\n2g2f 8c8d 0 0 1\n")),
            db_book(&format!("sfen {HIRATE}\n7g7f 3c3d 0 0 1\n")),
        ];
        let options = BookOptions::default();
        assert_eq!(best_and_ponder(&books, HIRATE, &options), Some(("2g2f".into(), "8c8d".into())));
        assert_eq!(
            best_and_ponder(&books[2..], HIRATE, &options),
            Some(("7g7f".into(), "3c3d".into()))
        );
        assert_eq!(best_and_ponder(&books[..1], HIRATE, &options), None);
    }

    #[test]
    fn priority_books_try_flipped_position_before_next_book() {
        let after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
        let flipped = flip::flipped_key(after_76).unwrap();
        let books = [
            db_book(&format!("sfen {flipped}\n7g7f none 0 0 1\n")),
            db_book(&format!("sfen {after_76}\n8c8d none 0 0 1\n")),
        ];
        let run = |flipped_book| {
            let options = BookOptions {
                flipped_book,
                ..Default::default()
            };
            best_and_ponder(&books, after_76, &options).map(|(best, _)| best)
        };
        assert_eq!(run(true).as_deref(), Some("3c3d"));
        assert_eq!(run(false).as_deref(), Some("8c8d"));
    }

    #[test]
    fn priority_books_do_not_fall_through_when_candidates_are_filtered_out() {
        // 手が全て下限未満でも、指し手なし (none) の行だけでも、局面は載っているので次へ進まない。
        let fallback = || db_book(&format!("sfen {HIRATE}\n7g7f none 50 0 1\n"));
        let options = BookOptions::default();
        assert!(best_and_ponder(&[fallback()], HIRATE, &options).is_some());
        for first in ["2g2f none -500 0 1", "none none 0 0 1"] {
            let books = [db_book(&format!("sfen {HIRATE}\n{first}\n")), fallback()];
            assert_eq!(best_and_ponder(&books, HIRATE, &options), None, "{first}");
        }
    }

    #[test]
    fn priority_books_complete_ponder_in_priority_order() {
        let after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
        let options = BookOptions::default();
        let first = format!("sfen {HIRATE}\n7g7f none 0 0 1\n");
        let books = [
            db_book(&first),
            db_book(&format!("sfen {after_76}\n3c3d none 0 0 1\n")),
        ];
        assert_eq!(best_and_ponder(&books, HIRATE, &options), Some(("7g7f".into(), "3c3d".into())));
        let books = [
            db_book(&format!("{first}sfen {after_76}\n8c8d none 0 0 1\n")),
            db_book(&format!("sfen {after_76}\n3c3d none 0 0 1\n")),
        ];
        assert_eq!(best_and_ponder(&books, HIRATE, &options), Some(("7g7f".into(), "8c8d".into())));
    }
}
