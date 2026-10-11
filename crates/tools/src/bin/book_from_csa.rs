//! CSA 棋譜群から YANEURAOU-DB2016 テキスト `.db` 定跡を生成するツール。
//!
//! floodgate 等の CSA 棋譜を再帰的に走査し、序盤の指し手を集計して定跡 DB を書き出す。
//! 生成した `.db` は `rshogi-book` のリーダ(`Book::from_path`)でそのまま読める。
//!
//! # 設計の要点
//!
//! - 出力は**常に新規スタンドアロン `.db`**。既存 book への追記・マージはしない
//!   (どの手を入れるかは生成時フラグで制御し、統合は別工程)。
//! - 「定跡手」の判定は**消費時間**で行う: 各手番側の初手から連続して消費時間が
//!   閾値以下(即指し)である間をその側の定跡内プレフィックスとみなす。
//! - 決定性(同一入力→出力 byte 一致)を担保する。集計は可換な加算のみで行い、
//!   出力生成は SFEN 昇順・指し手ソートで完全に順序づける。
//!
//! # 使い方
//!
//! ```bash
//! cargo run --release -p tools --bin book_from_csa -- \
//!     --root /path/to/csa --min-rating 4000 --out book.db
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use rshogi_core::position::Position as CorePosition;
use rshogi_core::types::{Color as CoreColor, Move};
use rshogi_csa::{
    CsaSummary, ParsedMove, SpecialMove, SummaryResult, csa_move_to_usi, parse_csa_full,
};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// 平手初期局面の SFEN(ply 込み)。この局面から始まらない棋譜は集計対象外とする。
const HIRATE_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

/// YANEURAOU-DB2016 テキスト `.db` のヘッダ行。
const DB_HEADER: &str = "#YANEURAOU-DB2016 1.00";

#[derive(Parser)]
#[command(
    name = "book_from_csa",
    version,
    about = "CSA 棋譜群から YANEURAOU-DB2016 テキスト定跡 .db を生成する"
)]
struct Cli {
    /// CSA 棋譜のルートディレクトリ(`*.csa` を再帰走査。各ディレクトリ内をソート)
    #[arg(long)]
    root: String,
    /// 出力先 `.db`(常に新規スタンドアロン。既存ファイルは上書き)
    #[arg(long)]
    out: String,
    /// 両対局者の対局時レート下限(0=無効)。0 超でレート情報が無い棋譜は除外
    #[arg(long, default_value_t = 0)]
    min_rating: u32,
    /// この手数までの局面・指し手のみ集計
    #[arg(long, default_value_t = 32)]
    max_ply: i32,
    /// 消費時間による定跡手判定の閾値(秒)。この秒数以下を「即指し」とみなす
    #[arg(long, default_value_t = 1)]
    instant_threshold_sec: u32,
    /// 消費時間による定跡手判定を無効化し、max-ply までの全手を集計する
    #[arg(long)]
    no_instant_filter: bool,
    /// 勝者側が指した手のみ集計する(引き分け・結果不明の棋譜は除外)
    #[arg(long)]
    winner_only: bool,
    /// 集計対象の手番
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    /// 集計後、採択回数がこの値未満の指し手を出力から除外する
    #[arg(long, default_value_t = 1)]
    min_count: u64,
}

/// 集計対象の手番(CLI 引数)。
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum SideArg {
    /// 両者
    Both,
    /// 先手のみ
    Black,
    /// 後手のみ
    White,
}

impl SideArg {
    /// 指定手番 `s` を集計対象に含めるか。
    fn includes(self, s: Side) -> bool {
        match self {
            SideArg::Both => true,
            SideArg::Black => s == Side::Black,
            SideArg::White => s == Side::White,
        }
    }
}

/// 生成時オプション(CLI から組み立て、集計ロジックに渡す純粋な設定)。
struct BuildOptions {
    min_rating: u32,
    max_ply: i32,
    /// 消費時間による定跡手判定を行うか。
    instant_filter: bool,
    instant_threshold_sec: u32,
    winner_only: bool,
    side: SideArg,
    min_count: u64,
}

/// 手番(集計内部表現。`Color` の別名だが 0/1 添字と相手取得を提供する)。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Side {
    Black,
    White,
}

impl Side {
    /// 手番ごとの状態配列用の添字(先手=0、後手=1)。
    fn idx(self) -> usize {
        match self {
            Side::Black => 0,
            Side::White => 1,
        }
    }

    /// 相手番。
    fn opponent(self) -> Side {
        match self {
            Side::Black => Side::White,
            Side::White => Side::Black,
        }
    }
}

/// `rshogi-core` の `Color` を内部 `Side` へ変換する。
fn side_of(c: CoreColor) -> Side {
    match c {
        CoreColor::Black => Side::Black,
        CoreColor::White => Side::White,
    }
}

/// 棋譜の結果(勝敗)。`--winner-only` の判定に使う。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Outcome {
    /// 勝者が確定している。
    Winner(Side),
    /// 引き分け or 結果不明(winner-only では棋譜ごと除外)。
    DrawOrUnknown,
}

/// 棋譜の `'summary:` 行、無ければ通常手の手数と末尾の特殊手から勝敗を判定する。
///
/// `'summary:` 行(wdoor / csa_client の棋譜)は勝敗を明示するので最優先する。wdoor の棋譜は
/// 反則負け・連続王手の千日手を特殊手なしで終え、異常終了の末尾は手番と勝敗が対応しない
/// ため、特殊手と手番からの推定では勝敗を誤る。
///
/// `'summary:` が無い棋譜は、平手前提(初手は先手)で通常手 `num_normal` 手後の手番側が特殊手を
/// 宣言した側とみなす。投了・時間切れ・反則は手番側の負け、入玉宣言勝ちは手番側の勝ち、
/// 千日手・持将棋等は引き分け、中断・特殊手なしは結果不明として扱う。
fn game_outcome(
    num_normal: usize,
    special: Option<&SpecialMove>,
    summary: Option<&CsaSummary>,
) -> Outcome {
    if let Some(summary) = summary {
        return match summary.result {
            SummaryResult::BlackWin => Outcome::Winner(Side::Black),
            SummaryResult::WhiteWin => Outcome::Winner(Side::White),
            SummaryResult::Draw => Outcome::DrawOrUnknown,
        };
    }
    // 通常手 num_normal 手後に手番が回っている側(= 特殊手を指した側)。
    let side_at_special = if num_normal.is_multiple_of(2) {
        Side::Black
    } else {
        Side::White
    };
    match special {
        Some(SpecialMove::Resign | SpecialMove::TimeUp | SpecialMove::IllegalMove) => {
            Outcome::Winner(side_at_special.opponent())
        }
        Some(SpecialMove::Win) => Outcome::Winner(side_at_special),
        Some(
            SpecialMove::Draw
            | SpecialMove::Sennichite
            | SpecialMove::Jishogi
            | SpecialMove::MaxMoves,
        ) => Outcome::DrawOrUnknown,
        Some(SpecialMove::Interrupt) | None => Outcome::DrawOrUnknown,
    }
}

/// SFEN 文字列から末尾の手数(ply)を落とす。末尾トークンが数値でなければそのまま返す。
///
/// 盤面(手番・持ち駒込み)だけをキーにして「同一局面(ply 除く)」を集約するために使う。
fn strip_ply(sfen: &str) -> &str {
    match sfen.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => sfen,
    }
}

/// 1 つの (局面, 指し手) の集計。
#[derive(Default)]
struct MoveAgg {
    /// 採択回数。
    count: u64,
    /// 直後に相手が指した手(USI)→ 回数。ponder の決定に使う。
    ponder_counts: HashMap<String, u64>,
}

impl MoveAgg {
    /// 別の集計を可換に足し込む。
    fn merge_from(&mut self, other: MoveAgg) {
        self.count += other.count;
        for (usi, c) in other.ponder_counts {
            *self.ponder_counts.entry(usi).or_insert(0) += c;
        }
    }

    /// 最頻の ponder(USI)を決める。同数タイは USI の辞書順で先のものを採用(決定性)。
    /// データが無ければ `None`。
    fn best_ponder(&self) -> Option<&str> {
        self.ponder_counts
            .iter()
            // count 最大 → タイは USI が辞書順で小さい方を「大きい」とみなして採用。
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(usi, _)| usi.as_str())
    }
}

/// 1 局面(ply を除いた盤面キー)の集計。
struct PositionAgg {
    /// 出力の sfen 行に使う最小 ply(同一盤面が複数 ply で現れたら最小に集約)。
    min_ply: i32,
    /// 指し手(USI)→ 集計。
    moves: HashMap<String, MoveAgg>,
}

impl PositionAgg {
    fn new(ply: i32) -> Self {
        Self {
            min_ply: ply,
            moves: HashMap::new(),
        }
    }

    /// 別の局面集計を可換に足し込む。
    fn merge_from(&mut self, other: PositionAgg) {
        self.min_ply = self.min_ply.min(other.min_ply);
        for (usi, magg) in other.moves {
            self.moves.entry(usi).or_default().merge_from(magg);
        }
    }
}

/// 走査サマリのカウンタ。
#[derive(Default, Clone)]
struct Stats {
    total_games: u64,
    parse_error: u64,
    non_hirate: u64,
    /// min_rating>0 でレート情報が無く除外した棋譜数。
    no_rating: u64,
    /// min_rating 未満で除外した棋譜数。
    rating_skipped: u64,
    /// winner-only で引き分け・結果不明のため除外した棋譜数。
    winner_skipped: u64,
    /// 実際に 1 手以上を集計に寄与した棋譜数。
    games_used: u64,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.total_games += o.total_games;
        self.parse_error += o.parse_error;
        self.non_hirate += o.non_hirate;
        self.no_rating += o.no_rating;
        self.rating_skipped += o.rating_skipped;
        self.winner_skipped += o.winner_skipped;
        self.games_used += o.games_used;
    }
}

/// 全体集計 + サマリ。可換なマージ(`merge`)で並列 fold/reduce できる。
#[derive(Default)]
struct Aggregator {
    /// ply を除いた盤面 SFEN(盤面 + 手番 + 持ち駒)→ 局面集計。
    positions: HashMap<String, PositionAgg>,
    stats: Stats,
}

impl Aggregator {
    /// `(局面キー, 手, ply, ponder)` を集計に足し込む。
    fn record(&mut self, key: &str, ply: i32, move_usi: &str, next_usi: Option<&str>) {
        let entry = self.positions.entry(key.to_string()).or_insert_with(|| PositionAgg::new(ply));
        entry.min_ply = entry.min_ply.min(ply);
        let magg = entry.moves.entry(move_usi.to_string()).or_default();
        magg.count += 1;
        if let Some(next) = next_usi {
            *magg.ponder_counts.entry(next.to_string()).or_insert(0) += 1;
        }
    }
}

/// 2 つの集計を可換にマージする(rayon reduce 用)。加算・min のみなので順序非依存。
fn merge(mut a: Aggregator, b: Aggregator) -> Aggregator {
    a.stats.add(&b.stats);
    for (key, pb) in b.positions {
        match a.positions.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().merge_from(pb),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(pb);
            }
        }
    }
    a
}

/// 1 手ぶんの再生記録(局面キー・指し手 USI・手番・消費時間)。
struct MoveRec {
    /// この手を指す直前の局面キー(ply を除いた SFEN)。
    key: String,
    /// この手の USI 文字列。
    usi: String,
    /// この手を指す手番側。
    side: Side,
    /// この手の消費時間(秒)。CSA に記載が無ければ `None`。
    time_sec: Option<u32>,
}

/// 1 棋譜を集計へ処理する。読み込み・パース失敗、平手でない、フィルタ除外は
/// サマリカウンタを進めて早期 return する。
fn process_game(path: &Path, opts: &BuildOptions, agg: &mut Aggregator) {
    agg.stats.total_games += 1;

    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            agg.stats.parse_error += 1;
            return;
        }
    };
    let (init_pos, parsed, info) = match parse_csa_full(&text) {
        Ok(r) => r,
        Err(_) => {
            agg.stats.parse_error += 1;
            return;
        }
    };

    // 平手初期局面から始まらない棋譜は対象外。
    if init_pos.to_sfen() != HIRATE_SFEN {
        agg.stats.non_hirate += 1;
        return;
    }

    // レートフィルタ(両者が min_rating 以上。0 超でレート欠落は除外)。
    if opts.min_rating > 0 {
        if info.black_rating.is_none() || info.white_rating.is_none() {
            agg.stats.no_rating += 1;
            return;
        }
        if !info.both_ratings_at_least(opts.min_rating as f64) {
            agg.stats.rating_skipped += 1;
            return;
        }
    }

    // 通常手列と末尾の特殊手を分離する。
    let mut normals = Vec::new();
    let mut special = None;
    for pm in parsed {
        match pm {
            ParsedMove::Normal(cm) => normals.push(cm),
            ParsedMove::Special(sp) => {
                special = Some(sp);
                break;
            }
        }
    }

    // 勝敗を判定し、winner-only の場合は勝者を確定(引き分け・不明は棋譜ごと除外)。
    let winner = if opts.winner_only {
        match game_outcome(normals.len(), special.as_ref(), info.summary.as_ref()) {
            Outcome::Winner(w) => Some(w),
            Outcome::DrawOrUnknown => {
                agg.stats.winner_skipped += 1;
                return;
            }
        }
    } else {
        None
    };

    // 棋譜を再生し、集計に必要な手数ぶんの記録を作る。
    // ponder は「その手の直後の相手手」なので、max_ply 手目の ponder 用に 1 手先まで USI を得る。
    let limit = normals.len().min(opts.max_ply.max(0) as usize + 1);
    let mut csa_pos = init_pos;
    let mut core_pos = CorePosition::new();
    if core_pos.set_sfen(HIRATE_SFEN).is_err() {
        agg.stats.parse_error += 1;
        return;
    }

    let mut recs: Vec<MoveRec> = Vec::with_capacity(limit);
    for csa_move in normals.iter().take(limit) {
        let side = side_of(core_pos.side_to_move());
        // CSA → USI 変換(変換は指す直前の局面を使う)。
        let usi = match csa_move_to_usi(&csa_move.mv, &csa_pos) {
            Ok(u) => u,
            Err(_) => break,
        };
        // core 局面での合法性検証 + 32bit 化(book の指し手は必ず検証してから使う)。
        let mv = match Move::from_usi(&usi).and_then(|d| core_pos.to_move(d)) {
            Some(m) if m != Move::NONE && core_pos.pseudo_legal(m) && core_pos.is_legal(m) => m,
            _ => break,
        };
        let key = strip_ply(&core_pos.to_sfen()).to_string();
        recs.push(MoveRec {
            key,
            usi,
            side,
            time_sec: csa_move.time_sec,
        });

        // 両局面を 1 手進める。core は検証済みなので安全に進められる。
        let gives_check = core_pos.gives_check(mv);
        core_pos.do_move(mv, gives_check);
        if csa_pos.apply_csa_move(&csa_move.mv).is_err() {
            // csa 側で適用に失敗したら以降の手は変換不能。ここで打ち切る。
            break;
        }
    }

    // 集計本体。手番ごとの定跡内プレフィックス状態を保ちながら 1 手ずつ判定する。
    let mut in_book = [true, true];
    let mut contributed = false;
    for (i, rec) in recs.iter().enumerate() {
        let ply = i as i32 + 1;
        if ply > opts.max_ply {
            break;
        }

        // 消費時間による定跡手判定(手番側ごとの即指しプレフィックス)。
        let in_prefix = if opts.instant_filter {
            let s = rec.side.idx();
            if !in_book[s] {
                false
            } else {
                let is_instant = rec.time_sec.is_some_and(|t| t <= opts.instant_threshold_sec);
                if !is_instant {
                    // この手で閾値超え → 以降その側は対象外。この手自身も定跡内ではない。
                    in_book[s] = false;
                }
                is_instant
            }
        } else {
            true
        };

        // 手番フィルタ + winner-only。
        let side_ok = opts.side.includes(rec.side) && winner.is_none_or(|w| w == rec.side);

        if !(in_prefix && side_ok) {
            continue;
        }

        let next_usi = recs.get(i + 1).map(|r| r.usi.as_str());
        agg.record(&rec.key, ply, &rec.usi, next_usi);
        contributed = true;
    }

    if contributed {
        agg.stats.games_used += 1;
    }
}

/// CSA ファイルを深さ優先でストリーミング走査する iterator。
///
/// 各ディレクトリの直下エントリだけをソートし、全ファイル一覧は保持しない。
struct CsaFileIter {
    stack: Vec<PathBuf>,
}

impl CsaFileIter {
    fn new(root: &Path) -> Self {
        Self {
            stack: vec![root.to_path_buf()],
        }
    }
}

impl Iterator for CsaFileIter {
    type Item = Result<PathBuf>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(path) = self.stack.pop() {
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e) => {
                    return Some(Err(e).with_context(|| {
                        format!("パスのメタデータ取得に失敗: {}", path.display())
                    }));
                }
            };
            if meta.is_dir() {
                let entries = match fs::read_dir(&path) {
                    Ok(entries) => entries
                        .map(|entry| {
                            entry.map(|e| e.path()).with_context(|| {
                                format!("ディレクトリエントリ読込に失敗: {}", path.display())
                            })
                        })
                        .collect::<Result<Vec<_>>>(),
                    Err(e) => Err(e)
                        .with_context(|| format!("ディレクトリ読込に失敗: {}", path.display())),
                };
                match entries {
                    Ok(mut entries) => {
                        entries.sort();
                        self.stack.extend(entries.into_iter().rev());
                    }
                    Err(e) => return Some(Err(e)),
                }
            } else if meta.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("csa"))
            {
                return Some(Ok(path));
            }
        }
        None
    }
}

/// `*.csa` を決定的な深さ優先順でストリーミング走査する。
fn visit_csa_files(root: &Path) -> CsaFileIter {
    CsaFileIter::new(root)
}

/// ルート配下の CSA 群を並列に集計する。
///
/// 各ワーカーは棋譜を 1 局ずつ fold し(全棋譜の load-all はしない)、可換な `merge` で reduce する。
/// 集計は加算・min のみなので分割の仕方に依らず結果は一意 = 決定的。
fn build_book(root: &Path, opts: &BuildOptions) -> Result<Aggregator> {
    eprintln!("CSA ファイルを {} からストリーミング走査", root.display());
    // par_bridge() は処理順を保証しない。各ディレクトリ内ソートで traversal 順は決定的にしつつ、
    // 出力の byte 一致は処理順ではなく、集計 merge が可換であることだけに依存させる。
    visit_csa_files(root)
        .par_bridge()
        .try_fold(Aggregator::default, |mut acc, path| {
            process_game(&path?, opts, &mut acc);
            Ok(acc)
        })
        .try_reduce(Aggregator::default, |a, b| Ok(merge(a, b)))
}

/// 集計結果を YANEURAOU-DB2016 テキスト `.db` として書き出す。返り値は (局面数, 指し手数)。
///
/// - sfen 行は文字列昇順でソート。
/// - 各局面の指し手は count 降順 → USI 昇順で安定に並べる。
/// - `min_count` 未満の手は除外し、手が空になった局面は出力しない。
fn write_book<W: Write>(
    agg: &Aggregator,
    opts: &BuildOptions,
    w: &mut W,
) -> io::Result<(usize, usize)> {
    writeln!(w, "{DB_HEADER}")?;

    // (出力 sfen, 局面集計) を作り、sfen 昇順でソート。
    let mut positions: Vec<(String, &PositionAgg)> = agg
        .positions
        .iter()
        .map(|(key, pagg)| (format!("{key} {}", pagg.min_ply), pagg))
        .collect();
    positions.sort_by(|a, b| a.0.cmp(&b.0));

    let mut n_positions = 0usize;
    let mut n_moves = 0usize;

    for (sfen, pagg) in &positions {
        // min_count でフィルタ。
        let mut moves: Vec<(&String, &MoveAgg)> =
            pagg.moves.iter().filter(|(_, magg)| magg.count >= opts.min_count).collect();
        if moves.is_empty() {
            continue;
        }
        // count 降順 → USI 昇順(決定的な並び)。
        moves.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(b.0)));

        writeln!(w, "sfen {sfen}")?;
        n_positions += 1;
        for (usi, magg) in &moves {
            let ponder = magg.best_ponder().unwrap_or("none");
            // value=0 depth=0 固定。count は集計値。
            writeln!(w, "{usi} {ponder} 0 0 {}", magg.count)?;
            n_moves += 1;
        }
    }

    Ok((n_positions, n_moves))
}

/// 走査・出力サマリを stderr に出力する。
fn print_summary(stats: &Stats, opts: &BuildOptions, n_positions: usize, n_moves: usize) {
    eprintln!("--- 集計サマリ ---");
    eprintln!("総棋譜数: {}", stats.total_games);
    eprintln!("  パース/読込エラー: {}", stats.parse_error);
    eprintln!("  平手でない: {}", stats.non_hirate);
    if opts.min_rating > 0 {
        eprintln!("  レート情報なしで除外: {}", stats.no_rating);
        eprintln!("  レート {} 未満で除外: {}", opts.min_rating, stats.rating_skipped);
    }
    if opts.winner_only {
        eprintln!("  引き分け/結果不明で除外(winner-only): {}", stats.winner_skipped);
    }
    eprintln!("  集計に寄与した棋譜: {}", stats.games_used);
    eprintln!("出力局面数: {n_positions}");
    eprintln!("出力指し手数: {n_moves}");
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let opts = BuildOptions {
        min_rating: cli.min_rating,
        max_ply: cli.max_ply,
        instant_filter: !cli.no_instant_filter,
        instant_threshold_sec: cli.instant_threshold_sec,
        winner_only: cli.winner_only,
        side: cli.side,
        min_count: cli.min_count,
    };

    let root = Path::new(&cli.root);
    let agg = build_book(root, &opts)?;

    let file = fs::File::create(&cli.out)
        .with_context(|| format!("出力ファイル生成に失敗: {}", cli.out))?;
    let mut w = BufWriter::new(file);
    let (n_positions, n_moves) = write_book(&agg, &opts, &mut w)?;
    w.flush()?;

    print_summary(&agg.stats, &opts, n_positions, n_moves);
    eprintln!("定跡を書き出しました: {}", cli.out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の既定オプション(instant フィルタ無効・全手集計)。
    fn opts_all() -> BuildOptions {
        BuildOptions {
            min_rating: 0,
            max_ply: 32,
            instant_filter: false,
            instant_threshold_sec: 1,
            winner_only: false,
            side: SideArg::Both,
            min_count: 1,
        }
    }

    /// 1 棋譜テキストを一時ディレクトリに書き、build_book で集計して出力バイト列を返す。
    fn build_to_bytes(games: &[(&str, &str)], opts: &BuildOptions) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in games {
            let mut f = fs::File::create(dir.path().join(name)).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        let agg = build_book(dir.path(), opts).unwrap();
        let mut buf = Vec::new();
        write_book(&agg, opts, &mut buf).unwrap();
        buf
    }

    fn out_string(games: &[(&str, &str)], opts: &BuildOptions) -> String {
        String::from_utf8(build_to_bytes(games, opts)).unwrap()
    }

    const AFTER_7G7F_W: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w";

    #[test]
    fn deterministic_byte_identical_across_runs() {
        // 複数棋譜・複数ファイルで 2 回実行し byte 一致を確認(rayon 分割に依らない決定性)。
        let games = &[
            ("a.csa", "V2.2\nPI\n+7776FU\n-3334FU\n+2726FU\n-8384FU\n%TORYO\n"),
            ("b.csa", "V2.2\nPI\n+7776FU\n-8384FU\n+2726FU\n-3334FU\n%TORYO\n"),
            ("c.csa", "V2.2\nPI\n+2726FU\n-3334FU\n+7776FU\n%TORYO\n"),
        ];
        let opts = opts_all();
        let a = build_to_bytes(games, &opts);
        let b = build_to_bytes(games, &opts);
        assert_eq!(a, b);
        // ヘッダと startpos 行があること。
        let s = String::from_utf8(a).unwrap();
        assert!(s.starts_with("#YANEURAOU-DB2016 1.00\n"));
        assert!(s.contains(&format!("sfen {HIRATE_SFEN}\n")));
    }

    #[test]
    fn instant_filter_excludes_side_after_slow_move() {
        // 先手は全て即指し(T0/T1)、後手は初手から長考(T5)。
        // 後手はプレフィックス開始前に閾値超え → 後手の手は 1 つも集計されない。
        let games =
            &[("g.csa", "V2.2\nPI\n+7776FU,T1\n-3334FU,T5\n+2726FU,T0\n-8384FU,T0\n%TORYO\n")];
        let opts = BuildOptions {
            instant_filter: true,
            instant_threshold_sec: 1,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        // 先手の手(startpos 7g7f、2 手進んだ局面で 2g2f)は入る。
        assert!(s.contains(&format!("sfen {HIRATE_SFEN}\n")));
        assert!(s.contains("7g7f "));
        assert!(s.contains("2g2f "));
        // 後手の手(7g7f 後の後手番局面)は集計されない → その局面行が出力に無い。
        assert!(
            !s.contains(&format!("sfen {AFTER_7G7F_W}")),
            "後手の手が除外されず局面が出力された:\n{s}"
        );
    }

    #[test]
    fn winner_only_counts_only_winner_moves() {
        // 3 手 + %TORYO: 後手番で投了 → 先手勝ち。先手の手のみ集計。
        let games = &[("g.csa", "V2.2\nPI\n+7776FU\n-3334FU\n+2726FU\n%TORYO\n")];
        let opts = BuildOptions {
            winner_only: true,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        assert!(s.contains("7g7f "), "先手初手が入るはず:\n{s}");
        assert!(s.contains("2g2f "), "先手 3 手目が入るはず:\n{s}");
        // 後手の 3c3d(7g7f 後の後手番局面)は除外。
        assert!(!s.contains(&format!("sfen {AFTER_7G7F_W}")), "後手手が入ってはいけない:\n{s}");
    }

    #[test]
    fn winner_only_excludes_draw_games() {
        // 千日手(%SENNICHITE)は引き分け → winner-only では棋譜ごと除外され出力が空。
        let games = &[("g.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%SENNICHITE\n")];
        let opts = BuildOptions {
            winner_only: true,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        assert!(!s.contains("sfen "), "引き分け棋譜が集計された:\n{s}");
    }

    #[test]
    fn max_ply_limits_counted_moves() {
        // max_ply=2 なら ply1,2 のみ集計。ply3 の局面(2 手進んだ盤面)は出ない。
        let games = &[("g.csa", "V2.2\nPI\n+7776FU\n-3334FU\n+2726FU\n-8384FU\n%TORYO\n")];
        let opts = BuildOptions {
            max_ply: 2,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        assert!(s.contains(&format!("sfen {HIRATE_SFEN}\n")), "startpos が必要:\n{s}");
        assert!(s.contains(&format!("sfen {AFTER_7G7F_W}")), "1 手進んだ局面が必要:\n{s}");
        // 2 手進んだ局面(先手番、7f/3d 済み)は ply3 なので出力されない。
        let after_2 = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b";
        assert!(!s.contains(after_2), "max_ply を超えた局面が出た:\n{s}");
    }

    #[test]
    fn min_rating_excludes_rateless_and_low() {
        // レート無し / 片方低い / 両者高い の 3 棋譜。min_rating=4000 で 1 局のみ採用。
        let no_rate = "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n";
        let low = "V2.2\nN+A\nN-B\n'black_rate:A+x:3000.0\n'white_rate:B+y:4500.0\nPI\n+2726FU\n-3334FU\n%TORYO\n";
        let high = "V2.2\nN+C\nN-D\n'black_rate:C+x:4200.0\n'white_rate:D+y:4100.0\nPI\n+7776FU\n-8384FU\n%TORYO\n";
        let games = &[("no.csa", no_rate), ("low.csa", low), ("high.csa", high)];
        let opts = BuildOptions {
            min_rating: 4000,
            ..opts_all()
        };
        let agg = {
            let dir = tempfile::tempdir().unwrap();
            for (n, b) in games {
                fs::write(dir.path().join(n), b).unwrap();
            }
            build_book(dir.path(), &opts).unwrap()
        };
        assert_eq!(agg.stats.no_rating, 1);
        assert_eq!(agg.stats.rating_skipped, 1);
        assert_eq!(agg.stats.games_used, 1);
        let mut buf = Vec::new();
        write_book(&agg, &opts, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // 採用された high 局面: startpos の 7g7f のみ(2726 は low 由来で除外済み)。
        assert!(s.contains("7g7f "));
        assert!(!s.contains("2g2f "), "レート未満棋譜の手が混入:\n{s}");
    }

    #[test]
    fn min_count_drops_rare_moves_and_empty_positions() {
        // startpos: 7g7f を 2 局、2g2f を 1 局。min_count=2 で 2g2f は消え、
        // 2g2f 始まりの局面は手が空になり出力されない。
        let games = &[
            ("a.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
            ("b.csa", "V2.2\nPI\n+7776FU\n-8384FU\n%TORYO\n"),
            ("c.csa", "V2.2\nPI\n+2726FU\n-3334FU\n%TORYO\n"),
        ];
        let opts = BuildOptions {
            min_count: 2,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        assert!(s.contains("7g7f "), "count>=2 の手は残る:\n{s}");
        assert!(!s.contains("2g2f "), "count<2 の手は消える:\n{s}");
        // 生き残る局面は startpos のみ(後手番局面は全て count1 の手だけなので空になり消える)。
        assert_eq!(s.matches("sfen ").count(), 1, "空局面が出力された:\n{s}");
    }

    #[test]
    fn ponder_is_most_frequent_reply() {
        // startpos 7g7f への相手応手: 3c3d 2 回、8c8d 1 回 → ponder=3c3d。
        let games = &[
            ("a.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
            ("b.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
            ("c.csa", "V2.2\nPI\n+7776FU\n-8384FU\n%TORYO\n"),
        ];
        let s = out_string(games, &opts_all());
        // startpos 行の直後の 7g7f 行に ponder=3c3d, count=3。
        assert!(s.contains("7g7f 3c3d 0 0 3"), "ponder 最頻が反映されない:\n{s}");
    }

    #[test]
    fn ponder_tie_breaks_by_usi_order() {
        // 3c3d と 8c8d が 1 回ずつ(タイ)→ USI 辞書順で先の 3c3d を採用。
        let games = &[
            ("a.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
            ("b.csa", "V2.2\nPI\n+7776FU\n-8384FU\n%TORYO\n"),
        ];
        let s = out_string(games, &opts_all());
        assert!(s.contains("7g7f 3c3d 0 0 2"), "タイ時に辞書順先頭が選ばれない:\n{s}");
    }

    #[test]
    fn ponder_none_when_no_reply() {
        // 1 手だけの棋譜(相手応手なし)→ ponder=none。
        let games = &[("a.csa", "V2.2\nPI\n+7776FU\n%TORYO\n")];
        let s = out_string(games, &opts_all());
        assert!(s.contains("7g7f none 0 0 1"), "応手なしで none にならない:\n{s}");
    }

    #[test]
    fn min_ply_aggregates_same_board() {
        // 飛車の往復で平手盤面が ply1 と ply5 の 2 回出る。ply 除去キーで 1 エントリに集約され、
        // 出力 sfen は最小 ply=1。startpos に 2h3h(ply1)と 7g7f(ply5)の 2 手が入る。
        let game = "V2.2\nPI\n+2838HI\n-8272HI\n+3828HI\n-7282HI\n+7776FU\n%TORYO\n";
        let games = &[("a.csa", game)];
        let s = out_string(games, &opts_all());
        // 平手盤面(ply1)の sfen 行がちょうど 1 つ。
        let count = s.matches(&format!("sfen {HIRATE_SFEN}\n")).count();
        assert_eq!(count, 1, "同一盤面が最小 ply に集約されていない:\n{s}");
        assert!(s.contains("2h3h "), "ply1 の手:\n{s}");
        assert!(s.contains("7g7f "), "ply5 の手が最小 ply 局面に集約されるはず:\n{s}");
    }

    #[test]
    fn roundtrip_probe_returns_expected_move() {
        // 生成した .db を rshogi-book で読み戻し、startpos の probe で 7g7f が返ること。
        use rshogi_book::{BookOptions, DefaultBookRng, probe};
        use rshogi_core::position::Position;

        let games = &[
            ("a.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
            ("b.csa", "V2.2\nPI\n+7776FU\n-3334FU\n%TORYO\n"),
        ];
        let opts = opts_all();
        let dir = tempfile::tempdir().unwrap();
        for (n, b) in games {
            fs::write(dir.path().join(n), b).unwrap();
        }
        let agg = build_book(dir.path(), &opts).unwrap();
        let db_path = dir.path().join("book.db");
        {
            let mut w = BufWriter::new(fs::File::create(&db_path).unwrap());
            write_book(&agg, &opts, &mut w).unwrap();
            w.flush().unwrap();
        }

        let book = rshogi_book::Book::from_path(&db_path, false).unwrap();
        let mut pos = Position::new();
        pos.set_sfen(HIRATE_SFEN).unwrap();
        let mut rng = DefaultBookRng::from_seed(1);
        let result = probe(&book, &pos, &BookOptions::default(), &mut rng, |_| {}).unwrap();
        assert_eq!(result.best_move.to_usi(), "7g7f");
        // ponder はこの棋譜集合では 3c3d。
        assert_eq!(result.ponder_move.map(|m| m.to_usi()).as_deref(), Some("3c3d"));
    }

    #[test]
    fn side_filter_black_only() {
        let games = &[("g.csa", "V2.2\nPI\n+7776FU\n-3334FU\n+2726FU\n%TORYO\n")];
        let opts = BuildOptions {
            side: SideArg::Black,
            ..opts_all()
        };
        let s = out_string(games, &opts);
        assert!(s.contains("7g7f "));
        assert!(s.contains("2g2f "));
        assert!(!s.contains(&format!("sfen {AFTER_7G7F_W}")), "後手手が入った:\n{s}");
    }

    #[test]
    fn game_outcome_mapping() {
        // 3 通常手後は後手番。投了 → 先手勝ち。
        assert_eq!(game_outcome(3, Some(&SpecialMove::Resign), None), Outcome::Winner(Side::Black));
        // 0 通常手で先手番。投了 → 後手勝ち。
        assert_eq!(game_outcome(0, Some(&SpecialMove::Resign), None), Outcome::Winner(Side::White));
        // 入玉宣言勝ちは手番側の勝ち。
        assert_eq!(game_outcome(2, Some(&SpecialMove::Win), None), Outcome::Winner(Side::Black));
        // 千日手・特殊手なしは引き分け/不明。
        assert_eq!(game_outcome(2, Some(&SpecialMove::Sennichite), None), Outcome::DrawOrUnknown);
        assert_eq!(game_outcome(2, None, None), Outcome::DrawOrUnknown);
    }

    #[test]
    fn game_outcome_prefers_summary_line() {
        let summary = |rest: &str| CsaSummary::parse(rest).unwrap();
        // 特殊手なし(wdoor の反則負け・連続王手の千日手)でも勝者が決まる。
        let s = summary("oute_sennichite:A lose:B win");
        assert_eq!(game_outcome(2, None, Some(&s)), Outcome::Winner(Side::White));
        // 手番からの推定(2 手後の %TORYO は後手勝ち)と食い違うときは summary が勝つ。
        let s = summary("abnormal:A win:B lose");
        assert_eq!(
            game_outcome(2, Some(&SpecialMove::Resign), Some(&s)),
            Outcome::Winner(Side::Black)
        );
        let s = summary("abnormal:A win:B lose");
        assert_eq!(
            game_outcome(2, Some(&SpecialMove::Interrupt), Some(&s)),
            Outcome::Winner(Side::Black)
        );
        let s = summary("sennichite:A draw:B draw");
        assert_eq!(game_outcome(2, Some(&SpecialMove::Resign), Some(&s)), Outcome::DrawOrUnknown);
    }

    #[test]
    fn winner_only_uses_summary_for_wdoor_style_endings() {
        // 連続王手の千日手で後手勝ち。終局コマンドは無い。先手の手は集計されない。
        let game = "V2\nN+A\nN-B\nPI\n+\n+7776FU\nT1\n-3334FU\nT1\n\
'summary:oute_sennichite:A lose:B win\n";
        let opts = BuildOptions {
            winner_only: true,
            ..opts_all()
        };
        let s = out_string(&[("g.csa", game)], &opts);
        assert!(s.contains("3c3d "), "後手(勝者)の手が入っていない:\n{s}");
        assert!(!s.contains("7g7f "), "先手(敗者)の手が入った:\n{s}");
    }
}
