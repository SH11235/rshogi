//! 入玉宣言・詰みの終盤 ground truth 指標の構築・採点ツール。
//!
//! 2 つの指標群を同じ bin に同居させる:
//!
//! - **宣言ルール距離ペア順序一致**（`build-pairs` / `eval-pairs`）: `%KACHI`（入玉宣言勝ち）
//!   で終局した対局の勝者手番局面から、「宣言成立へのルール距離が確定的に縮んだ」隣接局面
//!   pair を抽出し、NNUE 静的評価が後局面を前局面より高く評価するか（順序一致率）を
//!   条件別に測る。ルール特徴は `Position::entering_king_point_info` / `Position::in_check`
//!   のみを使い、評価値を母集団選別に使わない（循環の回避）。
//! - **探索読み切り詰み距離 concordance**（`build-mates` / `eval-mates`）: `%TORYO` で終局した
//!   対局の終盤勝者手番局面を oracle 探索（入玉宣言判定は無効化）にかけ、詰みを読み切れた
//!   局面（mate in N の通常探索結果）だけを採用し、NNUE 静的評価が詰み距離の順序と整合するか
//!   （concordance）と、詰み手を全合法手中の最善候補に挙げられるか（詰み手 top-1 率）を測る。
//!
//! 用語: 本ファイルの mate は「詰み（checkmate）」のみを指し、入玉宣言勝ちを含まない。
//! 入玉宣言勝ちは宣言ルール距離ペア側の担当で、両指標の母集団は終局特殊手
//! （`%KACHI` / `%TORYO`）で排他に分かれる。
//! CSA replay と終局特殊手の取得は `replay::csa_source::CsaSource` に委譲する。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rand_distr::{Binomial, Distribution};
use rayon::prelude::*;
use rshogi_core::eval::{MaterialLevel, set_material_level};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, LayerStacksAccCache,
    configure_layer_stack_routing, evaluate_dispatch, get_network, init_nnue,
    layer_stack_progress_coeff_required, load_progress_coeff_kpabs,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};
use rshogi_core::types::{Color, EnteringKingRule, Move, Value};
use rshogi_csa::SpecialMove;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::replay::csa_source::CsaSource;
use crate::replay::model::{
    GameIndex, GameIndexEntry, GameOutcomeView, GameSource, GameSourceRef, MoveView,
};
use crate::teacher_labeler::SEARCH_STACK_SIZE;

const DEFAULT_BOOTSTRAP: u32 = 10_000;
const DEFAULT_SEED: u64 = 20_260_726;
/// oracle 探索の worker ごとの置換表サイズ（MB）。局面ごとに `Search` を作り直すため
/// 過大にしない（`label_bench_positions` と同じ指針）。
const ORACLE_HASH_MB: usize = 64;

#[derive(Parser, Debug)]
#[command(
    name = "nyugyoku_metrics",
    version,
    about = "宣言ルール距離ペアと探索読み切り詰み距離を CSA から抽出し、NNUE 静的評価を採点する"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// %KACHI 終局対局の勝者手番局面から宣言ルール距離ペアを抽出する。
    BuildPairs(BuildArgs),
    /// pairs.jsonl を native NNUE 静的評価で採点し、順序一致率を条件別に集計する。
    EvalPairs(EvalArgs),
    /// %TORYO 終局対局の終盤勝者手番局面を oracle 探索し、詰み読み切り局面を抽出する。
    BuildMates(BuildMatesArgs),
    /// mates.jsonl を native NNUE 静的評価で採点し、詰み距離 concordance と詰み手 top-1 率を集計する。
    EvalMates(EvalMatesArgs),
}

#[derive(Parser, Debug)]
struct BuildArgs {
    /// 入力 CSA ファイルまたは CSA ディレクトリ。
    #[arg(long)]
    input: PathBuf,
    /// 出力ディレクトリ（pairs.jsonl / meta.json を書く）。
    #[arg(long)]
    out_dir: PathBuf,
}

#[derive(Parser, Debug)]
struct EvalArgs {
    /// `nyugyoku_metrics build-pairs` が出した pairs.jsonl。
    #[arg(long)]
    pairs: PathBuf,
    /// NNUE ファイル。
    #[arg(long)]
    eval_file: PathBuf,
    /// LayerStacks progresskpabs 用 progress.bin（--bucket-mode progresskpabs で必須）。
    #[arg(long)]
    progress_file: Option<PathBuf>,
    /// LayerStacks の bucket 選択モード。LayerStacks では必須。
    #[arg(long, value_enum)]
    bucket_mode: Option<BucketModeArg>,
    /// progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    progress_buckets: Option<usize>,
    /// metrics.json の出力先。
    #[arg(long)]
    out: PathBuf,
    /// 対局クラスタ bootstrap の replicate 数（0 で CI を出さない）。
    #[arg(long, default_value_t = DEFAULT_BOOTSTRAP)]
    bootstrap: u32,
    /// bootstrap 乱数 seed（同 seed・同入力で CI は bit 一致する）。
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// pair ごとの (eval_before, eval_after, agreement) を jsonl で出力する先。
    #[arg(long)]
    dump_pairs: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct BuildMatesArgs {
    /// 入力 CSA ファイルまたは CSA ディレクトリ。
    #[arg(long)]
    input: PathBuf,
    /// 出力ディレクトリ（mates.jsonl / meta.json を書く）。
    #[arg(long)]
    out_dir: PathBuf,
    /// 終局（最終手 = 勝者の手の局面を距離 0 とする）からこの手数未満の勝者手番局面を
    /// 候補にする。
    #[arg(long, default_value_t = 16)]
    tail_plies: u32,
    /// 候補の間引き刻み。終局からの手数距離 d が d % stride == 0 の勝者手番局面のみ
    /// 候補にする（勝者手番局面は 2 手ごとにしか現れないため、既定 2 で全候補）。
    #[arg(long, default_value_t = 2)]
    stride: u32,
    /// 走査する対局数の上限（決定的な走査順の先頭 N 対局。0 = 無制限）。
    #[arg(long, default_value_t = 0)]
    max_games: usize,
    /// oracle 探索の固定 depth。
    #[arg(long, default_value_t = 15)]
    oracle_depth: i32,
    /// oracle 探索のノード数上限（省略時は無制限）。depth と併用し、先に達した方で
    /// 打ち切る。
    #[arg(long)]
    oracle_nodes: Option<u64>,
    /// oracle 探索の worker スレッド数（0 = 利用可能 CPU 数）。並列化は局面単位で、
    /// 各局面の探索自体は 1 スレッド固定なので出力はスレッド数に依存しない。
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Parser, Debug)]
struct EvalMatesArgs {
    /// `nyugyoku_metrics build-mates` が出した mates.jsonl。
    #[arg(long)]
    mates: PathBuf,
    /// NNUE ファイル。
    #[arg(long)]
    eval_file: PathBuf,
    /// LayerStacks progresskpabs 用 progress.bin（--bucket-mode progresskpabs で必須）。
    #[arg(long)]
    progress_file: Option<PathBuf>,
    /// LayerStacks の bucket 選択モード。LayerStacks では必須。
    #[arg(long, value_enum)]
    bucket_mode: Option<BucketModeArg>,
    /// progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    progress_buckets: Option<usize>,
    /// metrics.json の出力先。
    #[arg(long)]
    out: PathBuf,
    /// 対局クラスタ bootstrap の replicate 数（0 で CI を出さない）。
    #[arg(long, default_value_t = DEFAULT_BOOTSTRAP)]
    bootstrap: u32,
    /// bootstrap 乱数 seed（同 seed・同入力で CI は bit 一致する）。
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// pair / 局面ごとの明細を jsonl で出力する先（行の `kind` で pair / position を区別）。
    #[arg(long)]
    dump: Option<PathBuf>,
}

/// LayerStacks bucket mode の CLI 表現。
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum BucketModeArg {
    /// 進行度方式（progress.bin 必須）。ek_testset eval と同じ既定。
    #[value(name = "progresskpabs")]
    ProgressKpabs,
    /// 両玉相対段方式（progress.bin 不要）。
    Kingrank9,
}

/// pair の遷移条件（本指標の 4 条件、固定順）。
///
/// いずれも「宣言成立へのルール距離が確定的に縮んだ」ことのルールベース判定で、
/// 評価値には依存しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Condition {
    /// 宣言点（27点法の駒点）が増えた。
    PointGain,
    /// 敵陣三段内の駒数（玉を除く）が増えた。
    ZonePieceGain,
    /// 勝者玉が敵陣三段内へ入った（false→true）。
    KingEntry,
    /// 勝者玉への王手が解除された（true→false）。
    CheckResolved,
}

impl Condition {
    const ALL: [Condition; 4] = [
        Condition::PointGain,
        Condition::ZonePieceGain,
        Condition::KingEntry,
        Condition::CheckResolved,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Condition::PointGain => "point_gain",
            Condition::ZonePieceGain => "zone_piece_gain",
            Condition::KingEntry => "king_entry",
            Condition::CheckResolved => "check_resolved",
        }
    }

    fn index(self) -> usize {
        match self {
            Condition::PointGain => 0,
            Condition::ZonePieceGain => 1,
            Condition::KingEntry => 2,
            Condition::CheckResolved => 3,
        }
    }
}

/// pairs.jsonl の 1 行（1 pair）。両局面とも勝者手番（同一 POV）。
#[derive(Debug, Serialize, Deserialize, Clone)]
struct PairRecord {
    source_csa: String,
    /// 勝者（宣言側）。'b' / 'w'。
    winner: char,
    ply_before: u32,
    ply_after: u32,
    sfen_before: String,
    sfen_after: String,
    /// 成立した遷移条件（1 つ以上）。
    conditions: Vec<Condition>,
    points_before: u32,
    points_after: u32,
    zone_before: u32,
    zone_after: u32,
    king_in_before: bool,
    king_in_after: bool,
    check_before: bool,
    check_after: bool,
}

#[derive(Debug, Serialize)]
struct BuildMeta {
    input: String,
    /// 走査した CSA ファイル数（1 ファイル = 1 対局）。
    games_scanned: usize,
    /// `%KACHI` 終局と確認できた対局数。
    kachi_games: usize,
    /// 勝者の突き合わせ（parse 由来の勝者 vs replay 終端手番）に失敗して除外した対局数。
    games_skipped_winner_mismatch: usize,
    /// replay 未完走・SFEN 復元不能などで終端局面を復元できず除外した対局数。
    games_skipped_broken: usize,
    /// `%KACHI` 記録だが終端局面で Point27 宣言が成立せず除外した対局数
    /// （宣言失敗 = illegal kachi、または別ルール運用の対局）。
    games_skipped_point27_mismatch: usize,
    /// 1 pair 以上を出力した対局数。
    games_with_pairs: usize,
    pairs_total: usize,
    /// 条件別の pair 数と対局数（クラスタ数）。
    conditions: BTreeMap<String, ConditionBuildCount>,
}

#[derive(Debug, Serialize)]
struct ConditionBuildCount {
    pairs: usize,
    games: usize,
}

/// eval-pairs の集計出力（全体 + 条件別）。
#[derive(Debug, Serialize)]
struct EvalPairsMetrics {
    pairs: String,
    eval_file: String,
    progress_file: Option<String>,
    bucket_mode: Option<String>,
    progress_buckets: Option<usize>,
    bootstrap: u32,
    seed: u64,
    overall: SliceMetrics,
    conditions: BTreeMap<String, SliceMetrics>,
}

/// 1 スライス（全体または 1 条件）の順序一致率と対局クラスタ bootstrap の 95% CI。
#[derive(Debug, Serialize, Clone, PartialEq)]
struct SliceMetrics {
    /// 順序一致率（tie は 0.5）。pair が 1 件も無いスライスは `None`。
    agreement: Option<f64>,
    n_pairs: u64,
    n_games: usize,
    ci95_lo: Option<f64>,
    ci95_hi: Option<f64>,
}

/// dump-pairs の 1 行。
#[derive(Debug, Serialize)]
struct DumpRecord<'a> {
    source_csa: &'a str,
    ply_before: u32,
    ply_after: u32,
    conditions: &'a [Condition],
    eval_before: i32,
    eval_after: i32,
    agreement: f64,
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::BuildPairs(args) => run_build(&args),
        Command::EvalPairs(args) => run_eval(&args),
        Command::BuildMates(args) => run_build_mates(&args),
        Command::EvalMates(args) => run_eval_mates(&args),
    }
}

// ---------------------------------------------------------------------------
// build-pairs
// ---------------------------------------------------------------------------

fn run_build(args: &BuildArgs) -> Result<()> {
    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", args.out_dir.display()))?;
    let pairs_path = args.out_dir.join("pairs.jsonl");
    let meta_path = args.out_dir.join("meta.json");
    let mut pairs_out = BufWriter::new(File::create(&pairs_path)?);

    let mut games_scanned = 0usize;
    let mut kachi_games = 0usize;
    let mut games_skipped_winner_mismatch = 0usize;
    let mut games_skipped_broken = 0usize;
    let mut games_skipped_point27_mismatch = 0usize;
    let mut games_with_pairs = 0usize;
    let mut pairs_total = 0usize;
    let mut cond_pairs = [0usize; Condition::ALL.len()];
    let mut cond_games = [0usize; Condition::ALL.len()];

    // 対局ごとの index やパス一覧をコーパス全体分保持しないよう、CSA を決定的な順序の
    // 遅延走査で 1 ファイル = 1 対局ずつ処理する（ピークメモリは対局数に非依存）。
    for path in csa_paths(&args.input)? {
        let path = path?;
        games_scanned += 1;

        let source = CsaSource::new(&path);
        let index = source.build_index()?;
        for warning in &index.warnings {
            eprintln!("warning: {warning}");
        }
        for entry in &index.entries {
            let game = source.load_game(&index, entry)?;
            // 宣言勝ち終局のみ対象（`%KACHI` がコメント等に現れただけのファイルはここで落ちる）。
            if game.termination != Some(SpecialMove::Win) {
                continue;
            }
            kachi_games += 1;
            let prov = pair_provenance(&index, entry)?;
            let source_csa = prov.source_csa.as_str();

            // 勝者 = 宣言側 = 終端で手番だった側（derive_outcome の Win 側と一致するはず）。
            let Some(GameOutcomeView::Win(winner)) = entry.outcome else {
                games_skipped_winner_mismatch += 1;
                eprintln!(
                    "warning: {source_csa}: %KACHI 終局なのに勝者を導出できないため除外します"
                );
                continue;
            };
            // 終端局面の突き合わせには replay の完走（全手が通常手として盤面追跡できたこと）
            // が必要。完走しなかった対局は終端局面を復元できないため broken として除外する。
            let replay_complete = game.moves.len() as u64 == u64::from(entry.ply_count)
                && game.moves.iter().all(|m| m.mv.is_normal());
            if !replay_complete {
                games_skipped_broken += 1;
                eprintln!(
                    "warning: {source_csa}: replay が完走せず終端局面を復元できないため除外します"
                );
                continue;
            }
            // 最終手（敗者の手のはず）と宣言側の突き合わせ。
            if let Some(last) = game.moves.last()
                && last.side == winner
            {
                games_skipped_winner_mismatch += 1;
                eprintln!("warning: {source_csa}: 宣言側と終端手番が一致しないため除外します");
                continue;
            }
            // wdoor shogi-server は宣言失敗（illegal kachi = 宣言側の負け）でも棋譜へ
            // `%KACHI` を書くため、終端局面（宣言側手番）で Point27 宣言が実際に成立するか
            // を評価し、成立しない対局は pair 化から除外する。
            let terminal = match terminal_position(&game.moves) {
                Ok(pos) => pos,
                Err(e) => {
                    games_skipped_broken += 1;
                    eprintln!(
                        "warning: {source_csa}: 終端局面を復元できないため除外します（{e:#}）"
                    );
                    continue;
                }
            };
            if terminal.declaration_win(EnteringKingRule::Point27) == Move::NONE {
                games_skipped_point27_mismatch += 1;
                eprintln!(
                    "warning: {source_csa}: %KACHI 記録だが終端局面で Point27 宣言が成立しないため除外します（宣言失敗または別ルールの対局）"
                );
                continue;
            }

            let pairs = match build_pairs_for_game(&game.moves, winner, &prov) {
                Ok(pairs) => pairs,
                Err(e) => {
                    games_skipped_broken += 1;
                    eprintln!("warning: {source_csa}: pair を構築できないため除外します（{e:#}）");
                    continue;
                }
            };
            if pairs.is_empty() {
                continue;
            }
            games_with_pairs += 1;
            let mut game_has_cond = [false; Condition::ALL.len()];
            for pair in &pairs {
                for cond in &pair.conditions {
                    cond_pairs[cond.index()] += 1;
                    game_has_cond[cond.index()] = true;
                }
                serde_json::to_writer(&mut pairs_out, pair)?;
                writeln!(pairs_out)?;
                pairs_total += 1;
            }
            for cond in Condition::ALL {
                if game_has_cond[cond.index()] {
                    cond_games[cond.index()] += 1;
                }
            }
        }
    }
    pairs_out.flush()?;

    let conditions = Condition::ALL
        .into_iter()
        .map(|cond| {
            (
                cond.as_str().to_string(),
                ConditionBuildCount {
                    pairs: cond_pairs[cond.index()],
                    games: cond_games[cond.index()],
                },
            )
        })
        .collect();
    let meta = BuildMeta {
        input: args.input.display().to_string(),
        games_scanned,
        kachi_games,
        games_skipped_winner_mismatch,
        games_skipped_broken,
        games_skipped_point27_mismatch,
        games_with_pairs,
        pairs_total,
        conditions,
    };
    write_json_pretty(&meta_path, &meta)?;

    eprintln!(
        "wrote {pairs_total} pairs from {games_with_pairs} games (kachi={kachi_games}, scanned={games_scanned}) to {}",
        pairs_path.display()
    );
    Ok(())
}

/// 入力がディレクトリなら配下の `*.csa` を、単一ファイルならそれ 1 つを列挙する。
///
/// ek_testset の同名ヘルパと同じ設計（ek_testset 側は変更しない方針のため複製）:
/// 全パスを収集・保持せず、ディレクトリごとにファイル名ソートした DFS で遅延走査する
/// （走査順は決定的、ピークメモリは総ファイル数に非依存）。`follow_links(false)` で
/// symlink は辿らない。走査エラーは握りつぶさず `Err` として返す。
fn csa_paths(input: &Path) -> Result<Box<dyn Iterator<Item = Result<PathBuf>>>> {
    let md = fs::metadata(input)
        .with_context(|| format!("入力を確認できません: {}", input.display()))?;
    if md.is_dir() {
        Ok(Box::new(
            WalkDir::new(input)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_map(|entry| match entry {
                    Ok(e) => (e.file_type().is_file()
                        && e.path().extension().and_then(|x| x.to_str()) == Some("csa"))
                    .then(|| Ok(e.into_path())),
                    Err(e) => Some(Err(anyhow!(e).context("入力ディレクトリの走査に失敗しました"))),
                }),
        ))
    } else {
        Ok(Box::new(std::iter::once(Ok(input.to_path_buf()))))
    }
}

/// `%KACHI` 対局の終端局面（宣言側手番）を、最終 normal 手を `sfen_before` へ適用して
/// 復元する。replay が完走していない（最終手が `Move::NONE` 等の）対局には使えない。
fn terminal_position(moves: &[MoveView]) -> Result<Position> {
    let last = moves.last().ok_or_else(|| anyhow!("指し手がありません"))?;
    if !last.mv.is_normal() {
        bail!("最終手が通常手ではありません");
    }
    let mut pos = Position::new();
    pos.set_sfen(&last.sfen_before)
        .with_context(|| format!("SFEN を復元できません: {}", last.sfen_before))?;
    let gives_check = pos.gives_check(last.mv);
    pos.do_move(last.mv, gives_check);
    Ok(pos)
}

/// 局面のルール特徴（すべて勝者視点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionFeatures {
    points: u32,
    zone_pieces: u32,
    king_in_enemy: bool,
    in_check: bool,
}

fn position_features(sfen: &str, winner: Color) -> Result<PositionFeatures> {
    let mut pos = Position::new();
    pos.set_sfen(sfen).with_context(|| format!("SFEN を復元できません: {sfen}"))?;
    if pos.side_to_move() != winner {
        bail!("勝者手番のはずの局面で手番が一致しません: {sfen}");
    }
    let info = pos.entering_king_point_info(winner);
    Ok(PositionFeatures {
        points: info.points,
        zone_pieces: info.enemy_zone_pieces,
        king_in_enemy: info.king_in_enemy,
        // 勝者手番なので in_check() は勝者玉への王手状態。
        in_check: pos.in_check(),
    })
}

/// 成立した遷移条件を固定順で返す（1 つも無ければ空）。
fn transition_conditions(before: &PositionFeatures, after: &PositionFeatures) -> Vec<Condition> {
    let mut out = Vec::new();
    if after.points > before.points {
        out.push(Condition::PointGain);
    }
    if after.zone_pieces > before.zone_pieces {
        out.push(Condition::ZonePieceGain);
    }
    if !before.king_in_enemy && after.king_in_enemy {
        out.push(Condition::KingEntry);
    }
    if before.in_check && !after.in_check {
        out.push(Condition::CheckResolved);
    }
    out
}

/// 隣接する勝者手番局面 pair の moves index (i, i+2) を列挙する。
///
/// 採用条件: 両端が勝者手番・ply 差がちょうど 2・間に相手の通常手が 1 手だけ挟まる。
/// 前局面の手または間の相手手が `Move::NONE`（parse fallback）の場合は盤面遷移を
/// 信頼できないので skip する（後局面は `sfen_before` しか使わないため手自体は不問）。
fn adjacent_winner_pair_indices(moves: &[MoveView], winner: Color) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, w) in moves.windows(3).enumerate() {
        let [a, mid, b] = w else {
            continue;
        };
        if a.side != winner || b.side != winner || mid.side == winner {
            continue;
        }
        if !a.mv.is_normal() || !mid.mv.is_normal() {
            continue;
        }
        // ply 欠番（PSV 由来等）は隣接とみなさない。
        if mid.ply.checked_sub(a.ply) != Some(1) || b.ply.checked_sub(a.ply) != Some(2) {
            continue;
        }
        out.push((i, i + 2));
    }
    out
}

/// 1 対局から条件付き pair を構築する（条件が 1 つも成立しない pair は出力しない）。
fn build_pairs_for_game(
    moves: &[MoveView],
    winner: Color,
    prov: &PairProvenance,
) -> Result<Vec<PairRecord>> {
    let mut out = Vec::new();
    for (i, j) in adjacent_winner_pair_indices(moves, winner) {
        let before = position_features(&moves[i].sfen_before, winner)?;
        let after = position_features(&moves[j].sfen_before, winner)?;
        let conditions = transition_conditions(&before, &after);
        if conditions.is_empty() {
            continue;
        }
        out.push(PairRecord {
            source_csa: prov.source_csa.clone(),
            winner: color_label(winner),
            ply_before: moves[i].ply,
            ply_after: moves[j].ply,
            sfen_before: moves[i].sfen_before.clone(),
            sfen_after: moves[j].sfen_before.clone(),
            conditions,
            points_before: before.points,
            points_after: after.points,
            zone_before: before.zone_pieces,
            zone_after: after.zone_pieces,
            king_in_before: before.king_in_enemy,
            king_in_after: after.king_in_enemy,
            check_before: before.in_check,
            check_after: after.in_check,
        });
    }
    Ok(out)
}

/// pair 出力へ記録する対局単位の出典情報。
#[derive(Debug, Clone)]
struct PairProvenance {
    source_csa: String,
}

fn pair_provenance(index: &GameIndex, entry: &GameIndexEntry) -> Result<PairProvenance> {
    let GameSourceRef::Csa { file_idx, .. } = entry.source else {
        bail!("CSA 以外の GameIndexEntry が渡されました");
    };
    let meta = index
        .pair_file(file_idx)
        .ok_or_else(|| anyhow!("file_idx {file_idx} が index にありません"))?;
    Ok(PairProvenance {
        source_csa: meta.path.display().to_string(),
    })
}

fn color_label(c: Color) -> char {
    match c {
        Color::Black => 'b',
        Color::White => 'w',
    }
}

fn color_from_label(c: char) -> Result<Color> {
    match c {
        'b' => Ok(Color::Black),
        'w' => Ok(Color::White),
        _ => bail!("winner は 'b' か 'w' を指定してください: {c:?}"),
    }
}

// ---------------------------------------------------------------------------
// eval-pairs
// ---------------------------------------------------------------------------

/// 集計スロット数（0 = 全体、1.. = `Condition::ALL` の各条件）。
const SLOTS: usize = Condition::ALL.len() + 1;

/// 対局 1 件の (agreement 和, pair 数)。対局クラスタ bootstrap の resample 単位。
#[derive(Debug, Clone, Copy, Default)]
struct GameAgg {
    sum: f64,
    count: u64,
}

#[derive(Debug, Clone, Copy)]
struct BootstrapReplicate {
    sum: f64,
    count: u64,
    remaining_draws: u64,
}

/// 対局クラスタ bootstrap を対局単位で逐次更新する。
///
/// 各 replicate の復元抽出回数は multinomial 分布に従う。現在の対局の抽出回数を
/// `Binomial(remaining_draws, 1 / remaining_games)` で条件付き生成すると、全対局の
/// 集計を保持せずに通常の n-out-of-n cluster bootstrap と同じ分布を得られる。
/// メモリ使用量は replicate 数にのみ比例し、対局数には依存しない。
struct StreamingBootstrap {
    rng: ChaCha8Rng,
    remaining_games: u64,
    replicates: Vec<BootstrapReplicate>,
}

impl StreamingBootstrap {
    fn new(n_games: usize, replicates: u32, seed: u64) -> Self {
        let replicate_count = if n_games == 0 { 0 } else { replicates as usize };
        Self {
            rng: ChaCha8Rng::seed_from_u64(seed),
            remaining_games: n_games as u64,
            replicates: vec![
                BootstrapReplicate {
                    sum: 0.0,
                    count: 0,
                    remaining_draws: n_games as u64,
                };
                replicate_count
            ],
        }
    }

    fn push(&mut self, game: GameAgg) -> Result<()> {
        if game.count == 0 {
            bail!("bootstrap に pair 数 0 の対局が渡されました");
        }
        if self.remaining_games == 0 {
            bail!("事前走査より多い対局が bootstrap に渡されました");
        }

        let probability = 1.0 / self.remaining_games as f64;
        for replicate in &mut self.replicates {
            let weight = if self.remaining_games == 1 {
                replicate.remaining_draws
            } else {
                Binomial::new(replicate.remaining_draws, probability)
                    .map_err(|e| anyhow!("bootstrap の二項分布を作れません: {e}"))?
                    .sample(&mut self.rng)
            };
            replicate.sum += game.sum * weight as f64;
            replicate.count += game.count * weight;
            replicate.remaining_draws -= weight;
        }
        self.remaining_games -= 1;
        Ok(())
    }

    fn finish(self) -> Result<Option<(f64, f64)>> {
        if self.remaining_games != 0 {
            bail!(
                "bootstrap の対局数が事前走査と一致しません（未処理 {} 対局）",
                self.remaining_games
            );
        }
        if self.replicates.is_empty() {
            return Ok(None);
        }

        let mut stats = Vec::with_capacity(self.replicates.len());
        for replicate in self.replicates {
            if replicate.remaining_draws != 0 || replicate.count == 0 {
                bail!("bootstrap replicate の復元抽出が完了していません");
            }
            stats.push(replicate.sum / replicate.count as f64);
        }
        stats.sort_by(f64::total_cmp);
        Ok(Some((percentile_sorted(&stats, 0.025), percentile_sorted(&stats, 0.975))))
    }
}

struct SliceAggregator {
    sum: f64,
    count: u64,
    n_games: usize,
    bootstrap: StreamingBootstrap,
}

impl SliceAggregator {
    fn new(n_games: usize, replicates: u32, seed: u64) -> Self {
        Self {
            sum: 0.0,
            count: 0,
            n_games: 0,
            bootstrap: StreamingBootstrap::new(n_games, replicates, seed),
        }
    }

    fn push(&mut self, game: GameAgg) -> Result<()> {
        self.sum += game.sum;
        self.count += game.count;
        self.n_games += 1;
        self.bootstrap.push(game)
    }

    fn finish(self) -> Result<SliceMetrics> {
        let ci = self.bootstrap.finish()?;
        Ok(SliceMetrics {
            agreement: (self.count > 0).then(|| self.sum / self.count as f64),
            n_pairs: self.count,
            n_games: self.n_games,
            ci95_lo: ci.map(|(lo, _)| lo),
            ci95_hi: ci.map(|(_, hi)| hi),
        })
    }
}

/// pair を対局クラスタ単位で集計する。保持するのは現在処理中の対局と bootstrap
/// replicate の状態だけで、完了済み対局の集計は保持しない。
struct PairAggregator {
    slots: [SliceAggregator; SLOTS],
}

impl PairAggregator {
    fn new(n_games: [usize; SLOTS], replicates: u32, seed: u64) -> Self {
        Self {
            slots: std::array::from_fn(|slot| {
                SliceAggregator::new(n_games[slot], replicates, slot_seed(seed, slot))
            }),
        }
    }

    fn push_game(&mut self, game: [GameAgg; SLOTS]) -> Result<()> {
        for (slot, aggregate) in game.into_iter().enumerate() {
            if aggregate.count > 0 {
                self.slots[slot].push(aggregate)?;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<(SliceMetrics, BTreeMap<String, SliceMetrics>)> {
        let [
            overall,
            point_gain,
            zone_piece_gain,
            king_entry,
            check_resolved,
        ] = self.slots;
        let overall = overall.finish()?;
        let conditions = [
            (Condition::PointGain, point_gain),
            (Condition::ZonePieceGain, zone_piece_gain),
            (Condition::KingEntry, king_entry),
            (Condition::CheckResolved, check_resolved),
        ]
        .into_iter()
        .map(|(condition, aggregate)| Ok((condition.as_str().to_string(), aggregate.finish()?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
        Ok((overall, conditions))
    }
}

/// スロット別 bootstrap seed の派生。
///
/// `seed + slot` のような加算派生は、近い seed 同士（例: `seed=42, slot=1` と
/// `seed=43, slot=0`）でストリームが衝突しうるため、黄金比由来の大定数
/// （splitmix64 の増分）を乗じてビット全域へ拡散させる。slot 0（全体）は `seed` のまま。
fn slot_seed(seed: u64, slot: usize) -> u64 {
    seed ^ (slot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// 昇順ソート済み標本の分位点。0 始まりの index `round((n - 1) * q)` の要素を返す
/// （round は四捨五入 = 半端は 0 から遠い側へ。nearest-rank 法 `ceil(n * q) - 1` とは
/// 異なり、線形補間もしない）。
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 順序一致スコア。後局面（距離小）を前局面より高く評価すれば 1、tie は 0.5。
fn agreement_score(eval_before: i32, eval_after: i32) -> f64 {
    match eval_after.cmp(&eval_before) {
        std::cmp::Ordering::Greater => 1.0,
        std::cmp::Ordering::Equal => 0.5,
        std::cmp::Ordering::Less => 0.0,
    }
}

/// jsonl を streaming で読み、非空行ごとに `visit(record, 1 始まり行番号)` を呼ぶ。
/// pairs.jsonl（`PairRecord`）と mates.jsonl（`MateRecord`）で共用する。
fn visit_jsonl_records<T: serde::de::DeserializeOwned>(
    path: &Path,
    mut visit: impl FnMut(T, usize) -> Result<()>,
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("入力を開けません: {}", path.display()))?;
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let line_no = line_no + 1;
        let record: T = serde_json::from_str(&line)
            .with_context(|| format!("{}:{line_no}: JSON を読めません", path.display()))?;
        visit(record, line_no)?;
    }
    Ok(())
}

/// `build-pairs` が出す source_csa 順の並びを一度走査し、全体・条件別の対局数を数える。
/// 二度目の走査ではこの件数を使い、完了済み対局を保持せず bootstrap を逐次更新する。
fn count_game_clusters(path: &Path) -> Result<[usize; SLOTS]> {
    let mut counts = [0usize; SLOTS];
    let mut current_source: Option<String> = None;
    let mut has_slot = [false; SLOTS];

    visit_jsonl_records::<PairRecord>(path, |pair, line_no| {
        if current_source.as_deref() != Some(pair.source_csa.as_str()) {
            if let Some(previous) = &current_source {
                if pair.source_csa < *previous {
                    bail!(
                        "{}:{line_no}: source_csa は昇順かつ対局単位で連続している必要があります",
                        path.display()
                    );
                }
                for (slot, present) in has_slot.into_iter().enumerate() {
                    counts[slot] += usize::from(present);
                }
            }
            current_source = Some(pair.source_csa.clone());
            has_slot = [false; SLOTS];
        }
        has_slot[0] = true;
        for condition in &pair.conditions {
            has_slot[condition.index() + 1] = true;
        }
        Ok(())
    })?;

    if current_source.is_some() {
        for (slot, present) in has_slot.into_iter().enumerate() {
            counts[slot] += usize::from(present);
        }
    }
    Ok(counts)
}

/// NNUE アーキテクチャと CLI 指定から、LayerStacks の実効 bucket mode を決める。
fn resolve_bucket_mode(
    is_layer_stacks: bool,
    bucket_mode: Option<BucketModeArg>,
    has_progress_file: bool,
    progress_buckets: Option<usize>,
) -> Result<Option<BucketModeArg>> {
    if !is_layer_stacks {
        if bucket_mode.is_some() {
            bail!("--bucket-mode は LayerStacks NNUE でのみ使用できます");
        }
        if has_progress_file {
            bail!("--progress-file は LayerStacks NNUE でのみ使用できます");
        }
        if progress_buckets.is_some() {
            bail!("--progress-buckets は LayerStacks NNUE でのみ使用できます");
        }
        return Ok(None);
    }

    let bucket_mode = bucket_mode.context("LayerStacks では --bucket-mode が必須です")?;
    match bucket_mode {
        BucketModeArg::ProgressKpabs
            if !has_progress_file && layer_stack_progress_coeff_required(progress_buckets) =>
        {
            bail!("--bucket-mode progresskpabs では --progress-file が必須です");
        }
        BucketModeArg::ProgressKpabs if progress_buckets.is_none() => {
            bail!("--bucket-mode progresskpabs では --progress-buckets が必須です");
        }
        BucketModeArg::Kingrank9 if has_progress_file => {
            bail!("--bucket-mode kingrank9 では --progress-file は使いません");
        }
        BucketModeArg::Kingrank9 if progress_buckets.is_some() => {
            bail!("--bucket-mode kingrank9 では --progress-buckets は使いません");
        }
        _ => {}
    }
    Ok(Some(bucket_mode))
}

/// eval-pairs / eval-mates 共通の評価器初期化。
///
/// `init_nnue` が対応する全アーキテクチャを読み込み、LayerStacks の場合だけ bucket mode
/// と progress 係数を設定して評価用 accumulator stack を返す。
fn init_eval(
    bucket_mode: Option<BucketModeArg>,
    progress_file: Option<&Path>,
    progress_buckets: Option<usize>,
    eval_file: &Path,
) -> Result<(AccumulatorStackVariant, Option<BucketModeArg>)> {
    init_nnue(eval_file)
        .with_context(|| format!("NNUE を読み込めません: {}", eval_file.display()))?;
    let network = get_network().ok_or_else(|| anyhow!("NNUE が初期化されていません"))?;
    let bucket_mode = resolve_bucket_mode(
        network.is_layer_stacks(),
        bucket_mode,
        progress_file.is_some(),
        progress_buckets,
    )?;

    match bucket_mode {
        Some(BucketModeArg::ProgressKpabs) => {
            if let Some(progress_file) = progress_file {
                let weights = load_progress_coeff_kpabs(progress_file)
                    .map_err(|e| anyhow!("progress 読み込みに失敗しました: {e}"))?;
                set_layer_stack_progress_kpabs_weights(weights)
                    .map_err(|e| anyhow!("progress 設定に失敗しました: {e}"))?;
            } else if layer_stack_progress_coeff_required(progress_buckets) {
                bail!("--bucket-mode progresskpabs では --progress-file が必須です");
            }
            configure_layer_stack_routing(
                LayerStackBucketMode::ProgressKPAbs,
                network.layer_stack_num_buckets().expect("LayerStacks checked"),
                progress_buckets,
            )
            .map_err(anyhow::Error::msg)?;
        }
        Some(BucketModeArg::Kingrank9) => {
            configure_layer_stack_routing(
                LayerStackBucketMode::KingRank9,
                network.layer_stack_num_buckets().expect("LayerStacks checked"),
                None,
            )
            .map_err(anyhow::Error::msg)?;
        }
        None => {}
    }
    Ok((AccumulatorStackVariant::from_network(&network), bucket_mode))
}

fn run_eval(args: &EvalArgs) -> Result<()> {
    let cluster_counts = count_game_clusters(&args.pairs)?;
    let (mut stack, bucket_mode) = init_eval(
        args.bucket_mode,
        args.progress_file.as_deref(),
        args.progress_buckets,
        &args.eval_file,
    )?;
    // acc_cache (Finny Tables) は静的 LayerStacks variant 専用の API で、
    // runtime-dimensions ビルドでは同じ net でも DynamicLayerStacks として load され
    // `as_layer_stacks()` が panic する (`is_layer_stacks()` は Dynamic でも true)。
    // `evaluate_dispatch` は None でも全 variant を正しく評価するため cache は使わない。
    let mut acc_cache: Option<LayerStacksAccCache> = None;

    let mut dump = args
        .dump_pairs
        .as_ref()
        .map(|path| {
            File::create(path)
                .map(BufWriter::new)
                .with_context(|| format!("出力できません: {}", path.display()))
        })
        .transpose()?;

    let mut agg = PairAggregator::new(cluster_counts, args.bootstrap, args.seed);
    let mut current_source: Option<String> = None;
    let mut current_game = [GameAgg::default(); SLOTS];
    visit_jsonl_records::<PairRecord>(&args.pairs, |pair, line_no| {
        let at = || format!("{}:{line_no}", args.pairs.display());
        if current_source.as_deref() != Some(pair.source_csa.as_str()) {
            if let Some(previous) = &current_source {
                if pair.source_csa < *previous {
                    bail!("{}: source_csa は昇順かつ対局単位で連続している必要があります", at());
                }
                agg.push_game(current_game)?;
            }
            current_source = Some(pair.source_csa.clone());
            current_game = [GameAgg::default(); SLOTS];
        }
        let winner = color_from_label(pair.winner).with_context(at)?;

        let mut eval_at = |sfen: &str| -> Result<i32> {
            let mut pos = Position::new();
            pos.set_sfen(sfen).with_context(|| format!("{}: SFEN を読めません", at()))?;
            if pos.side_to_move() != winner {
                bail!("{}: 勝者手番でない局面が pair に含まれています", at());
            }
            stack.reset();
            // ek_testset と同じく cp（歩=100）で扱う。順序一致は狭義単調変換に不変なので
            // fv_scale（定数倍）には依存しない。
            Ok(evaluate_dispatch(&pos, &mut stack, &mut acc_cache).to_cp())
        };
        let eval_before = eval_at(&pair.sfen_before)?;
        let eval_after = eval_at(&pair.sfen_after)?;
        let agreement = agreement_score(eval_before, eval_after);
        current_game[0].sum += agreement;
        current_game[0].count += 1;
        for condition in &pair.conditions {
            let aggregate = &mut current_game[condition.index() + 1];
            aggregate.sum += agreement;
            aggregate.count += 1;
        }

        if let Some(dump) = &mut dump {
            let record = DumpRecord {
                source_csa: &pair.source_csa,
                ply_before: pair.ply_before,
                ply_after: pair.ply_after,
                conditions: &pair.conditions,
                eval_before,
                eval_after,
                agreement,
            };
            serde_json::to_writer(&mut *dump, &record)?;
            writeln!(dump)?;
        }
        Ok(())
    })?;
    if current_source.is_some() {
        agg.push_game(current_game)?;
    }
    if let Some(mut dump) = dump {
        dump.flush()?;
    }

    let (overall, conditions) = agg.finish()?;
    let out = EvalPairsMetrics {
        pairs: args.pairs.display().to_string(),
        eval_file: args.eval_file.display().to_string(),
        progress_file: args.progress_file.as_ref().map(|p| p.display().to_string()),
        bucket_mode: bucket_mode
            .map(|mode| match mode {
                BucketModeArg::ProgressKpabs => LayerStackBucketMode::ProgressKPAbs.as_str(),
                BucketModeArg::Kingrank9 => LayerStackBucketMode::KingRank9.as_str(),
            })
            .map(str::to_string),
        progress_buckets: args.progress_buckets,
        bootstrap: args.bootstrap,
        seed: args.seed,
        overall,
        conditions,
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &out)?;
    writeln!(locked)?;
    write_json_pretty(&args.out, &out)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// build-mates
// ---------------------------------------------------------------------------

/// mates.jsonl の 1 行（勝者手番の詰み読み切り局面）。
#[derive(Debug, Serialize, Deserialize, Clone)]
struct MateRecord {
    source_csa: String,
    /// 勝者（詰ます側）。'b' / 'w'。
    winner: char,
    ply: u32,
    sfen: String,
    /// oracle の通常探索が読み切った詰み手数（勝者手番から数えた ply）。
    /// 最短および厳密な証明の保証はない。
    mate_in: u32,
    /// oracle 探索の最善手（USI）。
    oracle_bestmove: String,
    oracle_depth: i32,
    oracle_nodes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BuildMatesMeta {
    input: String,
    tail_plies: u32,
    stride: u32,
    max_games: usize,
    oracle_depth: i32,
    oracle_nodes: Option<u64>,
    /// 走査した CSA ファイル数（1 ファイル = 1 対局）。
    games_scanned: usize,
    /// `%TORYO` 終局と確認できた対局数。
    toryo_games: usize,
    /// 勝者の突き合わせ（parse 由来の勝者 vs 最終通常手の手番）に失敗して除外した対局数。
    games_skipped_winner_mismatch: usize,
    /// replay 未完走（`Move::NONE` 混入等）で盤面列を信頼できず除外した対局数。
    games_skipped_broken: usize,
    /// oracle 探索にかけた候補局面数（tail_plies / stride による選別後）。
    candidate_positions: usize,
    /// oracle が詰みを読み切り mates.jsonl に採用した局面数。
    mate_positions: usize,
    /// 採用局面を 1 つ以上持つ対局数。
    games_with_mates: usize,
}

/// oracle 探索が勝者の詰みを読み切ったときの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct OracleMate {
    mate_in: u32,
    bestmove_usi: String,
}

/// mate 帯（手番側の勝ち）の探索 score から「探索が発見した詰み手数（ply）」を取り出す。
///
/// - `is_win()` 帯（`Value::MATE_IN_MAX_PLY` 以上）のみ採用する。`is_loss()`（自玉が
///   詰まされる側）や通常評価値は `None`。
/// - `mate_ply() == 0`（`Value::MATE` ちょうど）は「root で既に勝ち」を意味し、通常の
///   詰み探索では現れない（EnteringKingRule 有効時の root 宣言勝ちスコアの形）。oracle は
///   宣言判定を無効化して探索するため実際には出ないはずだが、混入すると「詰み距離 0」の
///   偽ラベルになるため防御的に `None` にする。
fn winner_mate_in(score: Value) -> Option<u32> {
    if !score.is_win() {
        return None;
    }
    let ply = score.mate_ply();
    (ply >= 1).then_some(ply as u32)
}

/// 1 局面を固定 depth（必要ならノード数上限も併用）で探索し、score と最善手を返す
/// （内部用。rule を差し替え可能にしてあるのは EnteringKingRule::None の効果をテストで
/// 対照するため）。
///
/// 決定性の不変条件（`teacher_labeler` / `label_bench_positions` と同じ）:
/// 局面ごとに `Search` を作り直し 1 スレッド固定で探索する。これにより 1 局面の結果は
/// 他局面・処理順・worker スレッド数から独立し、同一入力なら出力が bit 一致する。
/// ノード上限も探索スレッドの決定的な node counter だけで判定され、時間には依存しない。
fn oracle_raw_search(
    sfen: &str,
    winner: Color,
    depth: i32,
    nodes: Option<u64>,
    rule: EnteringKingRule,
) -> Result<(Value, Move)> {
    let mut pos = Position::new();
    pos.set_sfen(sfen).with_context(|| format!("SFEN を復元できません: {sfen}"))?;
    if pos.side_to_move() != winner {
        bail!("勝者手番のはずの局面で手番が一致しません: {sfen}");
    }
    let mut search = Search::new(ORACLE_HASH_MB);
    search.set_num_threads(1);
    search.set_entering_king_rule(rule);
    let mut limits = LimitsType::default();
    limits.depth = depth;
    if let Some(nodes) = nodes {
        limits.nodes = nodes;
    }
    limits.set_start_time();
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
    Ok((result.score, result.best_move))
}

/// 1 候補局面を oracle 探索し、勝者の詰み読み切りなら `Some` を返す。
///
/// EnteringKingRule 有効の探索は root の宣言可能局面を mate 帯スコア（`Value::MATE` +
/// `Move::WIN`）で返しうる。本指標の mate は「詰み（checkmate）」のみで入玉宣言勝ちを
/// 含まないため、必ず `EnteringKingRule::None` で宣言判定を無効化して探索する。
/// さらに防御として、最善手が通常手でない結果（`Move::WIN` 等）は採用しない。
fn oracle_search_mate(
    sfen: &str,
    winner: Color,
    depth: i32,
    nodes: Option<u64>,
) -> Result<Option<OracleMate>> {
    let (score, best_move) = oracle_raw_search(sfen, winner, depth, nodes, EnteringKingRule::None)?;
    let Some(mate_in) = winner_mate_in(score) else {
        return Ok(None);
    };
    if !best_move.is_normal() {
        return Ok(None);
    }
    Ok(Some(OracleMate {
        mate_in,
        bestmove_usi: best_move.to_usi(),
    }))
}

/// 終局側 tail の勝者手番局面を候補として列挙する（`(ply, sfen_before)`、ply 昇順）。
///
/// 最終手（`%TORYO` 対局では勝者の手）の局面を距離 0 とし、距離 `d = last_ply - ply` が
/// `d < tail_plies` かつ `d % stride == 0` の勝者手番局面を採る。呼び出し側で replay
/// 完走（全手が通常手・ply 連番）を確認済みであること。
fn mate_candidates(
    moves: &[MoveView],
    winner: Color,
    tail_plies: u32,
    stride: u32,
) -> Vec<(u32, &str)> {
    let Some(last) = moves.last() else {
        return Vec::new();
    };
    let last_ply = last.ply;
    moves
        .iter()
        .filter(|m| m.side == winner)
        .filter(|m| {
            let d = last_ply - m.ply;
            d < tail_plies && d % stride == 0
        })
        .map(|m| (m.ply, m.sfen_before.as_str()))
        .collect()
}

fn run_build_mates(args: &BuildMatesArgs) -> Result<()> {
    if args.tail_plies == 0 {
        bail!("--tail-plies は 1 以上を指定してください");
    }
    if args.stride == 0 {
        bail!("--stride は 1 以上を指定してください");
    }
    // depth 0 以下は探索の停止条件が無くなる（時間管理探索へ落ちる）ため弾く。
    if args.oracle_depth <= 0 {
        bail!("--oracle-depth は 1 以上を指定してください");
    }
    if args.oracle_nodes == Some(0) {
        bail!("--oracle-nodes は 1 以上を指定してください（無制限は省略）");
    }
    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", args.out_dir.display()))?;
    let mates_path = args.out_dir.join("mates.jsonl");
    let meta_path = args.out_dir.join("meta.json");
    let mut mates_out = BufWriter::new(File::create(&mates_path)?);

    // mate 帯 score は通常探索の読み切り結果として使う。TT は 16-bit key のため理論上は
    // 衝突由来の偽 mate があり得る。同一 mates.jsonl を使うことで oracle の差はモデル間に
    // 混入しない。ただし、偽 mate や非最短距離を含む可能性による指標自体の偏りは排除できない。
    set_material_level(MaterialLevel::Lv1);

    let num_threads = if args.threads > 0 {
        args.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };
    // 並列化は局面単位（`par_iter` は順序保存 collect なので出力順も決定的）。
    // 深い探索の再帰スタック用に worker へ main 同等の 64MB を確保する。
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .stack_size(SEARCH_STACK_SIZE)
        .thread_name(|i| format!("oracle-worker-{i}"))
        .build()
        .context("rayon スレッドプールを構築できません")?;

    let mut games_scanned = 0usize;
    let mut toryo_games = 0usize;
    let mut games_skipped_winner_mismatch = 0usize;
    let mut games_skipped_broken = 0usize;
    let mut candidate_positions = 0usize;
    let mut mate_positions = 0usize;
    let mut games_with_mates = 0usize;

    // build-pairs と同じ決定的な遅延走査（ファイル名ソート、1 ファイル = 1 対局ずつ）。
    for path in csa_paths(&args.input)? {
        if args.max_games > 0 && games_scanned >= args.max_games {
            break;
        }
        let path = path?;
        games_scanned += 1;
        if games_scanned.is_multiple_of(1000) {
            eprintln!(
                "progress: {games_scanned} games scanned, {candidate_positions} candidates, {mate_positions} mates"
            );
        }

        let source = CsaSource::new(&path);
        let index = source.build_index()?;
        for warning in &index.warnings {
            eprintln!("warning: {warning}");
        }
        for entry in &index.entries {
            let game = source.load_game(&index, entry)?;
            // 投了終局のみ対象。宣言勝ち（%KACHI）は宣言ルール距離ペア側の担当で、
            // 本指標の mate（詰み）には含まない。
            if game.termination != Some(SpecialMove::Resign) {
                continue;
            }
            toryo_games += 1;
            let prov = pair_provenance(&index, entry)?;
            let source_csa = prov.source_csa.as_str();

            // 勝者 = 投了した側の相手（derive_outcome の Win 側）。
            let Some(GameOutcomeView::Win(winner)) = entry.outcome else {
                games_skipped_winner_mismatch += 1;
                eprintln!(
                    "warning: {source_csa}: %TORYO 終局なのに勝者を導出できないため除外します"
                );
                continue;
            };
            // 候補局面の SFEN 列を信頼するには replay の完走（全手が通常手として盤面追跡
            // できたこと）が必要。完走しなかった対局は除外する。
            let replay_complete = game.moves.len() as u64 == u64::from(entry.ply_count)
                && game.moves.iter().all(|m| m.mv.is_normal());
            if !replay_complete {
                games_skipped_broken += 1;
                eprintln!(
                    "warning: {source_csa}: replay が完走せず盤面列を信頼できないため除外します"
                );
                continue;
            }
            // 投了は手番側の行為なので、最終通常手は勝者の手のはず。不一致は除外する。
            if let Some(last) = game.moves.last()
                && last.side != winner
            {
                games_skipped_winner_mismatch += 1;
                eprintln!("warning: {source_csa}: 勝者と最終手番が一致しないため除外します");
                continue;
            }

            let candidates = mate_candidates(&game.moves, winner, args.tail_plies, args.stride);
            if candidates.is_empty() {
                continue;
            }
            candidate_positions += candidates.len();
            let oracle_results: Result<Vec<Option<OracleMate>>> = pool.install(|| {
                candidates
                    .par_iter()
                    .map(|(_, sfen)| {
                        oracle_search_mate(sfen, winner, args.oracle_depth, args.oracle_nodes)
                    })
                    .collect()
            });
            let oracle_results = match oracle_results {
                Ok(results) => results,
                Err(e) => {
                    games_skipped_broken += 1;
                    eprintln!(
                        "warning: {source_csa}: oracle 探索に失敗したため除外します（{e:#}）"
                    );
                    continue;
                }
            };

            let mut game_mates = 0usize;
            for ((ply, sfen), oracle) in candidates.iter().zip(oracle_results) {
                let Some(oracle) = oracle else {
                    continue;
                };
                let record = MateRecord {
                    source_csa: prov.source_csa.clone(),
                    winner: color_label(winner),
                    ply: *ply,
                    sfen: (*sfen).to_string(),
                    mate_in: oracle.mate_in,
                    oracle_bestmove: oracle.bestmove_usi,
                    oracle_depth: args.oracle_depth,
                    oracle_nodes: args.oracle_nodes,
                };
                serde_json::to_writer(&mut mates_out, &record)?;
                writeln!(mates_out)?;
                mate_positions += 1;
                game_mates += 1;
            }
            if game_mates > 0 {
                games_with_mates += 1;
            }
        }
    }
    mates_out.flush()?;

    let meta = BuildMatesMeta {
        input: args.input.display().to_string(),
        tail_plies: args.tail_plies,
        stride: args.stride,
        max_games: args.max_games,
        oracle_depth: args.oracle_depth,
        oracle_nodes: args.oracle_nodes,
        games_scanned,
        toryo_games,
        games_skipped_winner_mismatch,
        games_skipped_broken,
        candidate_positions,
        mate_positions,
        games_with_mates,
    };
    write_json_pretty(&meta_path, &meta)?;

    eprintln!(
        "wrote {mate_positions} mate positions from {games_with_mates} games (candidates={candidate_positions}, toryo={toryo_games}, scanned={games_scanned}) to {}",
        mates_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// eval-mates
// ---------------------------------------------------------------------------

/// mates.jsonl の事前走査で数えた対局クラスタ数（bootstrap の母数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MateClusterCounts {
    /// mate_in が厳密に異なる局面 pair を 1 組以上持つ対局数。
    concordance_games: usize,
    /// mate 局面を 1 つ以上持つ対局数。
    top1_games: usize,
}

/// `build-mates` が出す source_csa 順の並びを一度走査し、指標別の対局クラスタ数を数える。
fn count_mate_clusters(path: &Path) -> Result<MateClusterCounts> {
    let mut counts = MateClusterCounts {
        concordance_games: 0,
        top1_games: 0,
    };
    let mut current_source: Option<String> = None;
    let mut mate_ins: Vec<u32> = Vec::new();
    let mut flush = |mate_ins: &mut Vec<u32>| {
        if mate_ins.is_empty() {
            return;
        }
        counts.top1_games += 1;
        if mate_ins.iter().any(|&m| m != mate_ins[0]) {
            counts.concordance_games += 1;
        }
        mate_ins.clear();
    };
    visit_jsonl_records::<MateRecord>(path, |record, line_no| {
        if current_source.as_deref() != Some(record.source_csa.as_str()) {
            if let Some(previous) = &current_source {
                if record.source_csa < *previous {
                    bail!(
                        "{}:{line_no}: source_csa は昇順かつ対局単位で連続している必要があります",
                        path.display()
                    );
                }
                flush(&mut mate_ins);
            }
            current_source = Some(record.source_csa.clone());
        }
        mate_ins.push(record.mate_in);
        Ok(())
    })?;
    flush(&mut mate_ins);
    Ok(counts)
}

/// 詰み手 top-1 の判定に使う 1 局面ぶんの子局面評価の要約。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Top1Eval {
    /// top-1 スコア（厳密最大 1 / 最大タイ 0.5 / それ以外 0）。
    score: f64,
    /// oracle 最善手の子局面評価（指し手側視点 cp）。
    oracle_child_cp: i32,
    /// 全合法手中の最大子局面評価（指し手側視点 cp）。
    best_child_cp: i32,
    /// 合法手数。
    n_legal: usize,
    /// 最大値タイの手数（oracle 手を含む）。
    n_best: usize,
}

/// 詰み手 top-1 のスコア規約。`best_cp` は全合法手（oracle 手を含む）の最大値なので
/// `oracle_cp > best_cp` はあり得ない。
fn top1_score(oracle_cp: i32, best_cp: i32, n_best: usize) -> f64 {
    if oracle_cp < best_cp {
        0.0
    } else if n_best == 1 {
        1.0
    } else {
        0.5
    }
}

/// 1 mate 局面の全合法手を 1 手ずつ適用し、子局面の静的評価（相手番になるので符号反転
/// して指し手側視点 cp に揃える）で oracle 最善手が最大かを判定する。
///
/// 詰み手の子局面は相手玉が王手を受けた局面になるが、除外せず同じ規約で評価する
/// （「詰みに向かう手を静的評価だけで選べるか」を測るのが目的のため）。
fn mate_top1_eval(
    pos: &mut Position,
    oracle_bestmove: &str,
    stack: &mut AccumulatorStackVariant,
    acc_cache: &mut Option<LayerStacksAccCache>,
) -> Result<Top1Eval> {
    let mut list = MoveList::new();
    // oracle の最善手が不成などの搦め手でも必ず候補に含まれるよう、全合法手を生成する。
    generate_legal_all(pos, &mut list);
    if list.is_empty() {
        bail!("合法手がありません");
    }
    let mut best_cp = i32::MIN;
    let mut n_best = 0usize;
    let mut oracle_cp: Option<i32> = None;
    for &mv in list.iter() {
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        stack.reset();
        let cp = (-evaluate_dispatch(pos, stack, acc_cache)).to_cp();
        pos.undo_move(mv);
        match cp.cmp(&best_cp) {
            std::cmp::Ordering::Greater => {
                best_cp = cp;
                n_best = 1;
            }
            std::cmp::Ordering::Equal => n_best += 1,
            std::cmp::Ordering::Less => {}
        }
        if mv.to_usi() == oracle_bestmove {
            oracle_cp = Some(cp);
        }
    }
    let Some(oracle_cp) = oracle_cp else {
        bail!("oracle_bestmove {oracle_bestmove} が合法手にありません");
    };
    Ok(Top1Eval {
        score: top1_score(oracle_cp, best_cp, n_best),
        oracle_child_cp: oracle_cp,
        best_child_cp: best_cp,
        n_legal: list.len(),
        n_best,
    })
}

/// 同一対局内の mate 局面から、mate_in が厳密に異なる全 pair の順序一致を列挙する。
///
/// `items[i] = (mate_in, eval)`（eval は勝者手番視点 cp）。返り値は
/// `(near, far, agreement)` の列で、near = 詰みに近い側（mate_in 小）の index、
/// far = 遠い側の index。agreement は near の eval が far より高ければ 1、tie 0.5、逆 0。
fn concordance_pairs(items: &[(u32, i32)]) -> Vec<(usize, usize, f64)> {
    let mut out = Vec::new();
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            if items[i].0 == items[j].0 {
                continue;
            }
            let (near, far) = if items[i].0 < items[j].0 {
                (i, j)
            } else {
                (j, i)
            };
            let agreement = agreement_score(items[far].1, items[near].1);
            out.push((near, far, agreement));
        }
    }
    out
}

/// eval-mates の集計出力。
#[derive(Debug, Serialize)]
struct EvalMatesMetrics {
    mates: String,
    eval_file: String,
    progress_file: Option<String>,
    bucket_mode: Option<String>,
    progress_buckets: Option<usize>,
    bootstrap: u32,
    seed: u64,
    concordance: ConcordanceMetrics,
    mate_top1: MateTop1Metrics,
}

/// 詰み距離 concordance（同一対局内 pair の順序一致率）と対局クラスタ bootstrap 95% CI。
#[derive(Debug, Serialize, Clone, PartialEq)]
struct ConcordanceMetrics {
    /// 順序一致率（tie は 0.5）。pair が 1 組も無ければ `None`。
    agreement: Option<f64>,
    n_pairs: u64,
    n_games: usize,
    ci95_lo: Option<f64>,
    ci95_hi: Option<f64>,
}

/// 詰み手 top-1 率（厳密最大 1 / 最大タイ 0.5）と対局クラスタ bootstrap 95% CI。
#[derive(Debug, Serialize, Clone, PartialEq)]
struct MateTop1Metrics {
    /// top-1 率。局面が 1 つも無ければ `None`。
    rate: Option<f64>,
    n_positions: u64,
    n_games: usize,
    ci95_lo: Option<f64>,
    ci95_hi: Option<f64>,
}

/// `--dump` の pair 行。
#[derive(Debug, Serialize)]
struct MateDumpPair<'a> {
    kind: &'static str,
    source_csa: &'a str,
    /// 詰みに近い側（mate_in 小）。
    ply_near: u32,
    mate_in_near: u32,
    eval_near: i32,
    /// 詰みから遠い側（mate_in 大）。
    ply_far: u32,
    mate_in_far: u32,
    eval_far: i32,
    agreement: f64,
}

/// `--dump` の position 行。
#[derive(Debug, Serialize)]
struct MateDumpPosition<'a> {
    kind: &'static str,
    source_csa: &'a str,
    ply: u32,
    mate_in: u32,
    oracle_bestmove: &'a str,
    oracle_child_eval: i32,
    best_child_eval: i32,
    n_legal: usize,
    n_best: usize,
    score: f64,
}

/// 1 対局ぶんの mate 局面を評価し、concordance / top-1 の対局集計を返す。
/// dump が指定されていれば per-position / per-pair の明細行も書く。
fn process_mate_game(
    records: &[MateRecord],
    stack: &mut AccumulatorStackVariant,
    acc_cache: &mut Option<LayerStacksAccCache>,
    dump: &mut Option<BufWriter<File>>,
) -> Result<(GameAgg, GameAgg)> {
    let winner = color_from_label(records[0].winner)?;
    let mut items: Vec<(u32, i32)> = Vec::with_capacity(records.len());
    let mut top1_agg = GameAgg::default();
    for record in records {
        if record.winner != records[0].winner {
            bail!("{}: 同一対局内で winner が一致しません", record.source_csa);
        }
        let mut pos = Position::new();
        pos.set_sfen(&record.sfen)
            .with_context(|| format!("SFEN を復元できません: {}", record.sfen))?;
        if pos.side_to_move() != winner {
            bail!("{}: 勝者手番でない局面が含まれています: {}", record.source_csa, record.sfen);
        }
        // concordance は勝者手番視点の静的評価同士を比較する（eval-pairs と同じ規約）。
        stack.reset();
        let eval = evaluate_dispatch(&pos, stack, acc_cache).to_cp();
        let top1 = mate_top1_eval(&mut pos, &record.oracle_bestmove, stack, acc_cache)
            .with_context(|| format!("{} ply {}", record.source_csa, record.ply))?;
        top1_agg.sum += top1.score;
        top1_agg.count += 1;
        if let Some(dump) = dump {
            let row = MateDumpPosition {
                kind: "position",
                source_csa: &record.source_csa,
                ply: record.ply,
                mate_in: record.mate_in,
                oracle_bestmove: &record.oracle_bestmove,
                oracle_child_eval: top1.oracle_child_cp,
                best_child_eval: top1.best_child_cp,
                n_legal: top1.n_legal,
                n_best: top1.n_best,
                score: top1.score,
            };
            serde_json::to_writer(&mut *dump, &row)?;
            writeln!(dump)?;
        }
        items.push((record.mate_in, eval));
    }

    let mut concordance_agg = GameAgg::default();
    for (near, far, agreement) in concordance_pairs(&items) {
        concordance_agg.sum += agreement;
        concordance_agg.count += 1;
        if let Some(dump) = dump {
            let row = MateDumpPair {
                kind: "pair",
                source_csa: &records[near].source_csa,
                ply_near: records[near].ply,
                mate_in_near: records[near].mate_in,
                eval_near: items[near].1,
                ply_far: records[far].ply,
                mate_in_far: records[far].mate_in,
                eval_far: items[far].1,
                agreement,
            };
            serde_json::to_writer(&mut *dump, &row)?;
            writeln!(dump)?;
        }
    }
    Ok((concordance_agg, top1_agg))
}

fn run_eval_mates(args: &EvalMatesArgs) -> Result<()> {
    let cluster_counts = count_mate_clusters(&args.mates)?;
    let (mut stack, bucket_mode) = init_eval(
        args.bucket_mode,
        args.progress_file.as_deref(),
        args.progress_buckets,
        &args.eval_file,
    )?;
    // acc_cache を使わない理由は `init_eval` 呼び出し元の eval-pairs 側と同じ
    // （runtime-dimensions ビルドで `as_layer_stacks()` が panic するため）。
    let mut acc_cache: Option<LayerStacksAccCache> = None;

    let mut dump = args
        .dump
        .as_ref()
        .map(|path| {
            File::create(path)
                .map(BufWriter::new)
                .with_context(|| format!("出力できません: {}", path.display()))
        })
        .transpose()?;

    // 指標ごとに独立の対局クラスタ bootstrap を回す。concordance は seed そのまま、
    // top-1 は slot_seed で派生（eval-pairs の条件 slot と同じ衝突回避方式）。
    let mut concordance_agg =
        SliceAggregator::new(cluster_counts.concordance_games, args.bootstrap, args.seed);
    let mut top1_agg =
        SliceAggregator::new(cluster_counts.top1_games, args.bootstrap, slot_seed(args.seed, 1));

    // 対局単位で group 化して処理する。保持するのは現在処理中の対局の record 列だけ
    // （tail_plies / stride で上限が決まる少数）で、完了済み対局の集計は保持しない。
    let mut current_source: Option<String> = None;
    let mut current_records: Vec<MateRecord> = Vec::new();
    let mut flush_game = |records: &mut Vec<MateRecord>,
                          stack: &mut AccumulatorStackVariant,
                          acc_cache: &mut Option<LayerStacksAccCache>,
                          dump: &mut Option<BufWriter<File>>|
     -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let (concordance_game, top1_game) = process_mate_game(records, stack, acc_cache, dump)?;
        if concordance_game.count > 0 {
            concordance_agg.push(concordance_game)?;
        }
        top1_agg.push(top1_game)?;
        records.clear();
        Ok(())
    };
    visit_jsonl_records::<MateRecord>(&args.mates, |record, line_no| {
        if current_source.as_deref() != Some(record.source_csa.as_str()) {
            if let Some(previous) = &current_source {
                if record.source_csa < *previous {
                    bail!(
                        "{}:{line_no}: source_csa は昇順かつ対局単位で連続している必要があります",
                        args.mates.display()
                    );
                }
                flush_game(&mut current_records, &mut stack, &mut acc_cache, &mut dump)?;
            }
            current_source = Some(record.source_csa.clone());
        }
        current_records.push(record);
        Ok(())
    })?;
    flush_game(&mut current_records, &mut stack, &mut acc_cache, &mut dump)?;
    if let Some(mut dump) = dump {
        dump.flush()?;
    }

    let concordance = concordance_agg.finish()?;
    let top1 = top1_agg.finish()?;
    let out = EvalMatesMetrics {
        mates: args.mates.display().to_string(),
        eval_file: args.eval_file.display().to_string(),
        progress_file: args.progress_file.as_ref().map(|p| p.display().to_string()),
        bucket_mode: bucket_mode
            .map(|mode| match mode {
                BucketModeArg::ProgressKpabs => LayerStackBucketMode::ProgressKPAbs.as_str(),
                BucketModeArg::Kingrank9 => LayerStackBucketMode::KingRank9.as_str(),
            })
            .map(str::to_string),
        progress_buckets: args.progress_buckets,
        bootstrap: args.bootstrap,
        seed: args.seed,
        concordance: ConcordanceMetrics {
            agreement: concordance.agreement,
            n_pairs: concordance.n_pairs,
            n_games: concordance.n_games,
            ci95_lo: concordance.ci95_lo,
            ci95_hi: concordance.ci95_hi,
        },
        mate_top1: MateTop1Metrics {
            rate: top1.agreement,
            n_positions: top1.n_pairs,
            n_games: top1.n_games,
            ci95_lo: top1.ci95_lo,
            ci95_hi: top1.ci95_hi,
        },
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &out)?;
    writeln!(locked)?;
    write_json_pretty(&args.out, &out)?;
    Ok(())
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut writer = BufWriter::new(
        File::create(path).with_context(|| format!("出力できません: {}", path.display()))?,
    );
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::model::MoveAnnotation;

    #[test]
    fn layer_stacks_bucket_mode_is_explicit_and_validates_progress_options() {
        assert!(resolve_bucket_mode(true, None, true, Some(8)).is_err());
        assert_eq!(
            resolve_bucket_mode(true, Some(BucketModeArg::ProgressKpabs), true, Some(8))
                .expect("明示 progresskpabs"),
            Some(BucketModeArg::ProgressKpabs)
        );
        assert_eq!(
            resolve_bucket_mode(true, Some(BucketModeArg::Kingrank9), false, None)
                .expect("明示 kingrank9"),
            Some(BucketModeArg::Kingrank9)
        );

        let missing_progress =
            resolve_bucket_mode(true, Some(BucketModeArg::ProgressKpabs), false, Some(8))
                .expect_err("progresskpabs には progress-file が必要");
        assert!(missing_progress.to_string().contains("--progress-file が必須"));
        // routing bucket 数 1 は no-op routing のため progress-file なしでも通る。
        assert_eq!(
            resolve_bucket_mode(true, Some(BucketModeArg::ProgressKpabs), false, Some(1))
                .expect("N=1 は係数不要"),
            Some(BucketModeArg::ProgressKpabs)
        );

        let unused_progress = resolve_bucket_mode(true, Some(BucketModeArg::Kingrank9), true, None)
            .expect_err("kingrank9 では progress-file を拒否");
        assert!(unused_progress.to_string().contains("--progress-file は使いません"));
        assert!(resolve_bucket_mode(true, Some(BucketModeArg::ProgressKpabs), true, None).is_err());
    }

    #[test]
    fn non_layer_stacks_rejects_layer_stacks_options() {
        assert_eq!(resolve_bucket_mode(false, None, false, None).expect("専用 option なし"), None);

        let bucket_mode =
            resolve_bucket_mode(false, Some(BucketModeArg::ProgressKpabs), false, None)
                .expect_err("bucket-mode を拒否");
        assert!(bucket_mode.to_string().contains("--bucket-mode は LayerStacks"));
        let progress_file =
            resolve_bucket_mode(false, None, true, None).expect_err("progress-file を拒否");
        assert!(progress_file.to_string().contains("--progress-file は LayerStacks"));
        let both = resolve_bucket_mode(false, Some(BucketModeArg::Kingrank9), true, None)
            .expect_err("両 option を拒否");
        assert!(both.to_string().contains("--bucket-mode は LayerStacks"));
    }

    fn write_csa(dir: &Path, name: &str, text: &str) {
        let mut f = File::create(dir.join(name)).expect("create");
        f.write_all(text.as_bytes()).expect("write");
    }

    /// テスト用の出典情報（build_pairs_for_game 直呼び用）。
    fn test_prov(name: &str) -> PairProvenance {
        PairProvenance {
            source_csa: name.to_string(),
        }
    }

    /// 先手が王手解除 → 点数/枚数増 → %KACHI で勝つ合成対局。
    /// pair (1,3) = check_resolved、pair (3,5) = point_gain + zone_piece_gain。
    /// 終端（6 手目適用後、先手手番）は Point27 宣言成立形:
    /// 玉 42（敵陣内）・敵陣内 10 枚（飛 2 + 角 2 + 金 4 + 銀 1 + 歩 1）・
    /// 28 点（10 + 大駒加点 4x4 + 持ち歩 2）・王手なし。
    const KACHI_GAINS_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+51OU11HI21HI13KA23KA61KI71KI81KI91KI63GI34FU00FU00FU\n",
        "P-59OU42GI\n",
        "+\n",
        "+5141OU\nT1\n",
        "-4243GI\nT1\n",
        "+3433FU\nT1\n",
        "-4344GI\nT1\n",
        "+4142OU\nT1\n",
        "-4445GI\nT1\n",
        "%KACHI\n",
    );

    /// 先手玉が敵陣へ入城して %KACHI で勝つ合成対局。pair (1,3) = king_entry。
    /// 終端（4 手目適用後、先手手番）は Point27 宣言成立形:
    /// 玉 52（敵陣内）・敵陣内 10 枚（飛 2 + 角 2 + 金 4 + 銀 2）・
    /// 28 点（10 + 大駒加点 4x4 + 持ち歩 2）・王手なし。
    const KACHI_ENTRY_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU33KA43KA63HI73HI21KI31KI41KI51KI61GI71GI00FU00FU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-1918OU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    /// replay は完走するが、終端局面で Point27 宣言が成立しない %KACHI 対局
    /// （宣言失敗 = illegal kachi の棋譜を模す。玉のみで点数・枚数が全く足りない）。
    const KACHI_POINT27_FAIL_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-1918OU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    /// %KACHI だが間の相手手が不正（Move::NONE fallback + 打ち切り）で pair を出せない対局。
    const KACHI_BROKEN_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-5556FU\nT1\n",
        "+5352OU\nT1\n",
        "-1817OU\nT1\n",
        "%KACHI\n",
    );

    const TORYO_CSA: &str = "V2.2\nN+B\nN-W\nPI\n+7776FU\nT1\n-3334FU\nT1\n%TORYO\n";

    fn load_game_from(text: &str) -> (Vec<MoveView>, Color) {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let Some(GameOutcomeView::Win(winner)) = entry.outcome else {
            panic!("winner expected");
        };
        let game = source.load_game(&index, entry).expect("load_game");
        (game.moves, winner)
    }

    #[test]
    fn kachi_game_yields_condition_pairs() {
        let (moves, winner) = load_game_from(KACHI_GAINS_CSA);
        assert_eq!(winner, Color::Black);
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert_eq!(pairs.len(), 2);

        let first_pair = &pairs[0];
        assert_eq!((first_pair.ply_before, first_pair.ply_after), (1, 3));
        assert_eq!(first_pair.conditions, vec![Condition::CheckResolved]);
        assert!(first_pair.check_before && !first_pair.check_after);
        assert_eq!((first_pair.points_before, first_pair.points_after), (27, 27));

        let second_pair = &pairs[1];
        assert_eq!((second_pair.ply_before, second_pair.ply_after), (3, 5));
        assert_eq!(second_pair.conditions, vec![Condition::PointGain, Condition::ZonePieceGain]);
        assert_eq!((second_pair.points_before, second_pair.points_after), (27, 28));
        assert_eq!((second_pair.zone_before, second_pair.zone_after), (9, 10));
    }

    #[test]
    fn king_entry_is_detected() {
        let (moves, winner) = load_game_from(KACHI_ENTRY_CSA);
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].conditions, vec![Condition::KingEntry]);
        assert!(!pairs[0].king_in_before && pairs[0].king_in_after);
    }

    #[test]
    fn broken_intermediate_move_yields_no_pairs() {
        // 不正な相手手で replay が Move::NONE + 打ち切りになり、pair は 1 件も出ない。
        // 終端局面の復元も不能（最終手が通常手でない）。
        let (moves, winner) = load_game_from(KACHI_BROKEN_CSA);
        assert!(moves.iter().any(|m| !m.mv.is_normal()));
        assert!(terminal_position(&moves).is_err());
        let pairs = build_pairs_for_game(&moves, winner, &test_prov("game.csa")).expect("pairs");
        assert!(pairs.is_empty());
    }

    fn mv(ply: u32, side: Color, normal: bool) -> MoveView {
        MoveView {
            ply,
            side,
            sfen_before: String::new(),
            mv: if normal {
                Move::from_usi("7g7f").expect("usi")
            } else {
                Move::NONE
            },
            kif_label: String::new(),
            annotation: MoveAnnotation::default(),
        }
    }

    #[test]
    fn pair_indices_require_adjacent_plies_and_normal_moves() {
        let b = Color::Black;
        let w = Color::White;
        // 正常な並び: (0,2) と (2,4)。
        let moves = vec![
            mv(1, b, true),
            mv(2, w, true),
            mv(3, b, true),
            mv(4, w, true),
            mv(5, b, true),
        ];
        assert_eq!(adjacent_winner_pair_indices(&moves, b), vec![(0, 2), (2, 4)]);
        // winner が後手なら後手手番の窓 (1,3) だけが対象になる。
        assert_eq!(adjacent_winner_pair_indices(&moves, w), vec![(1, 3)]);

        // ply 欠番（3 の次が 6）: (0,2) のみ。
        let gapped = vec![
            mv(1, b, true),
            mv(2, w, true),
            mv(3, b, true),
            mv(6, w, true),
            mv(7, b, true),
        ];
        assert_eq!(adjacent_winner_pair_indices(&gapped, b), vec![(0, 2)]);

        // 間の相手手が Move::NONE: skip。
        let broken = vec![mv(1, b, true), mv(2, w, false), mv(3, b, true)];
        assert_eq!(adjacent_winner_pair_indices(&broken, b), Vec::new());

        // 前局面の勝者手が Move::NONE: skip。
        let broken_first = vec![mv(1, b, false), mv(2, w, true), mv(3, b, true)];
        assert_eq!(adjacent_winner_pair_indices(&broken_first, b), Vec::new());
    }

    #[test]
    fn run_build_writes_pairs_and_meta() {
        let in_dir = tempfile::tempdir().expect("tempdir");
        write_csa(in_dir.path(), "a_gains.csa", KACHI_GAINS_CSA);
        write_csa(in_dir.path(), "b_entry.csa", KACHI_ENTRY_CSA);
        write_csa(in_dir.path(), "c_toryo.csa", TORYO_CSA);
        write_csa(in_dir.path(), "d_broken.csa", KACHI_BROKEN_CSA);
        write_csa(in_dir.path(), "e_point27_fail.csa", KACHI_POINT27_FAIL_CSA);
        let out_dir = tempfile::tempdir().expect("tempdir");

        run_build(&BuildArgs {
            input: in_dir.path().to_path_buf(),
            out_dir: out_dir.path().to_path_buf(),
        })
        .expect("run_build");

        let pairs_text =
            fs::read_to_string(out_dir.path().join("pairs.jsonl")).expect("pairs.jsonl");
        let pairs: Vec<PairRecord> = pairs_text
            .lines()
            .map(|l| serde_json::from_str(l).expect("pair json"))
            .collect();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().all(|p| p.winner == 'b'));
        assert!(pairs.iter().all(|p| !p.conditions.is_empty()));
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out_dir.path().join("meta.json")).unwrap())
                .expect("meta json");
        assert_eq!(meta["games_scanned"], 5);
        // %TORYO の対局は kachi に数えない。
        assert_eq!(meta["kachi_games"], 4);
        // broken は replay 未完走（終端復元不能）として除外。
        assert_eq!(meta["games_skipped_broken"], 1);
        // %KACHI 記録でも終端で Point27 宣言が成立しない対局は除外して計上する。
        assert_eq!(meta["games_skipped_point27_mismatch"], 1);
        assert_eq!(meta["games_skipped_winner_mismatch"], 0);
        assert_eq!(meta["games_with_pairs"], 2);
        assert_eq!(meta["pairs_total"], 3);
        assert_eq!(meta["conditions"]["check_resolved"]["pairs"], 1);
        assert_eq!(meta["conditions"]["check_resolved"]["games"], 1);
        assert_eq!(meta["conditions"]["point_gain"]["pairs"], 1);
        assert_eq!(meta["conditions"]["zone_piece_gain"]["pairs"], 1);
        assert_eq!(meta["conditions"]["king_entry"]["pairs"], 1);
        assert_eq!(meta["conditions"]["king_entry"]["games"], 1);
    }

    #[test]
    fn agreement_scores_order_and_tie() {
        assert_eq!(agreement_score(100, 200), 1.0);
        assert_eq!(agreement_score(200, 200), 0.5);
        assert_eq!(agreement_score(200, 100), 0.0);
    }

    #[test]
    fn aggregator_computes_rates_per_condition() {
        let mut agg = PairAggregator::new([2, 1, 0, 1, 1], 0, DEFAULT_SEED);
        let mut first_game = [GameAgg::default(); SLOTS];
        first_game[0] = GameAgg { sum: 1.0, count: 2 };
        first_game[Condition::PointGain.index() + 1] = GameAgg { sum: 1.0, count: 2 };
        first_game[Condition::KingEntry.index() + 1] = GameAgg { sum: 0.0, count: 1 };
        agg.push_game(first_game).unwrap();

        let mut second_game = [GameAgg::default(); SLOTS];
        second_game[0] = GameAgg { sum: 1.0, count: 1 };
        second_game[Condition::CheckResolved.index() + 1] = GameAgg { sum: 1.0, count: 1 };
        agg.push_game(second_game).unwrap();
        let (overall, conditions) = agg.finish().unwrap();

        assert_eq!(overall.n_pairs, 3);
        assert_eq!(overall.n_games, 2);
        assert!((overall.agreement.unwrap() - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(overall.ci95_lo, None, "bootstrap=0 では CI を出さない");

        let pg = &conditions["point_gain"];
        assert_eq!((pg.n_pairs, pg.n_games), (2, 1));
        assert_eq!(pg.agreement, Some(0.5));
        let ke = &conditions["king_entry"];
        assert_eq!((ke.n_pairs, ke.n_games), (1, 1));
        assert_eq!(ke.agreement, Some(0.0));
        let zp = &conditions["zone_piece_gain"];
        assert_eq!((zp.n_pairs, zp.n_games), (0, 0));
        assert_eq!(zp.agreement, None);
    }

    fn streaming_ci(per_game: &[GameAgg], replicates: u32, seed: u64) -> Option<(f64, f64)> {
        let mut bootstrap = StreamingBootstrap::new(per_game.len(), replicates, seed);
        for game in per_game {
            bootstrap.push(*game).unwrap();
        }
        bootstrap.finish().unwrap()
    }

    #[test]
    fn percentile_uses_round_of_n_minus_one_times_q() {
        // index = round((n-1) * q) 方式の境界順位を直接固定する。
        let sorted: Vec<f64> = (0..10_000).map(f64::from).collect();
        // (10000-1) * 0.025 = 249.975 → round = 250（nearest-rank の ceil(n*q)-1 = 249 とは別）。
        assert_eq!(percentile_sorted(&sorted, 0.025), 250.0);
        // (10000-1) * 0.975 = 9749.025 → round = 9749。
        assert_eq!(percentile_sorted(&sorted, 0.975), 9749.0);
        // 端の q は端の要素。
        assert_eq!(percentile_sorted(&sorted, 0.0), 0.0);
        assert_eq!(percentile_sorted(&sorted, 1.0), 9999.0);
        // 単一要素は q に依らずその値。
        assert_eq!(percentile_sorted(&[0.5], 0.025), 0.5);
        assert_eq!(percentile_sorted(&[0.5], 0.975), 0.5);
    }

    #[test]
    fn slot_seeds_do_not_collide_across_adjacent_seeds() {
        // 旧方式 `seed + slot` は (seed=42, slot=1) と (seed=43, slot=0) が同一ストリーム
        // になっていた。大定数乗算の xor 派生で近傍 seed 間の衝突が無いことを固定する。
        for seed in [0u64, 42, DEFAULT_SEED] {
            for a in 0..SLOTS {
                for b in 0..SLOTS {
                    if a != b {
                        assert_ne!(slot_seed(seed, a), slot_seed(seed, b));
                    }
                    assert_ne!(slot_seed(seed, a), slot_seed(seed + 1, b));
                }
            }
        }
        // slot 0（全体）は seed をそのまま使う。
        assert_eq!(slot_seed(DEFAULT_SEED, 0), DEFAULT_SEED);
    }

    #[test]
    fn bootstrap_is_deterministic_for_same_seed() {
        let per_game = vec![
            GameAgg { sum: 3.0, count: 4 },
            GameAgg { sum: 1.0, count: 2 },
            GameAgg { sum: 0.5, count: 1 },
            GameAgg { sum: 5.0, count: 6 },
        ];
        let a = streaming_ci(&per_game, 1000, DEFAULT_SEED).expect("ci");
        let b = streaming_ci(&per_game, 1000, DEFAULT_SEED).expect("ci");
        assert_eq!(a, b, "同 seed 同入力で CI は bit 一致する");
        assert!(a.0 <= a.1);
        assert!((0.0..=1.0).contains(&a.0) && (0.0..=1.0).contains(&a.1));
    }

    #[test]
    fn bootstrap_single_game_collapses_to_point() {
        let per_game = vec![GameAgg { sum: 3.0, count: 4 }];
        let (lo, hi) = streaming_ci(&per_game, 100, DEFAULT_SEED).expect("ci");
        assert_eq!(lo, 0.75);
        assert_eq!(hi, 0.75);
    }

    // -----------------------------------------------------------------------
    // 探索読み切り詰み距離 concordance（build-mates / eval-mates）
    // -----------------------------------------------------------------------

    /// 先手番で先手が 1 手詰めの局面。5c の歩が 5b への金打ちを支え、G*5b が唯一の詰み。
    const MATE_IN_ONE_SFEN: &str = "4k4/9/4P4/9/9/9/9/9/4K4 b G 1";

    /// 先手番で先手が 3 手詰め（1 手詰めなし）の局面。
    /// G*5c（5d の歩が支える）に対し後手玉は 1 段目へ逃げるしかなく、どこへ逃げても
    /// もう 1 枚の金打ちで頭金の詰み。
    const MATE_IN_THREE_SFEN: &str = "9/4k4/9/4P4/9/9/9/9/4K4 b 2G 1";

    /// 先手番で Point27 宣言が成立する形だが、詰みは（浅い探索の範囲では）存在しない局面。
    /// 宣言要件: 先手玉 5b が敵陣内・敵陣内の玉以外の駒 10 枚（飛2 角2 金4 銀2）・
    /// 28 点（大駒 4x5 + 小駒 6 + 持ち歩 2）・王手なし。後手玉 1i は 7 段目の歩壁の
    /// 向こうにあり、先手の飛角は g 段の歩を突破しない限り王手すら掛からない。
    const DECLARATION_FORM_SFEN: &str = "R1GGG1G1R/B1S1K1S1B/9/9/9/9/ppppppppp/9/8k b 2P 1";

    /// oracle 系テストの前提となる評価器（NNUE 不要の駒得評価）を有効化する。
    /// `run_build_mates` が本番でやるのと同じ設定で、プロセスグローバルかつ冪等。
    fn enable_material_eval() {
        set_material_level(MaterialLevel::Lv1);
    }

    #[test]
    fn winner_mate_in_accepts_only_positive_mate_band() {
        // 通常の詰みスコア。
        assert_eq!(winner_mate_in(Value::mate_in(1)), Some(1));
        assert_eq!(winner_mate_in(Value::mate_in(15)), Some(15));
        // mate 帯の下端（MATE_IN_MAX_PLY ちょうど）は win 扱い。
        assert_eq!(
            winner_mate_in(Value::MATE_IN_MAX_PLY),
            Some(Value::MATE.raw() as u32 - Value::MATE_IN_MAX_PLY.raw() as u32)
        );
        // 下端より 1 小さいと mate 帯ではない。
        assert_eq!(winner_mate_in(Value::new(Value::MATE_IN_MAX_PLY.raw() - 1)), None);
        // Value::MATE ちょうど（mate_ply 0 = 宣言勝ちの root スコアの形）は不採用。
        assert_eq!(winner_mate_in(Value::MATE), None);
        // 詰まされ側（負の mate 帯）と通常評価値は不採用。
        assert_eq!(winner_mate_in(Value::mated_in(3)), None);
        assert_eq!(winner_mate_in(Value::new(300)), None);
        assert_eq!(winner_mate_in(Value::new(-300)), None);
    }

    #[test]
    fn mate_candidates_filters_by_tail_and_stride() {
        let b = Color::Black;
        let w = Color::White;
        // ply 1..=9、先手が奇数 ply、最終手（ply 9）は先手 = 勝者。
        let moves: Vec<MoveView> =
            (1..=9).map(|ply| mv(ply, if ply % 2 == 1 { b } else { w }, true)).collect();

        // tail 4: 距離 d = 9 - ply が {0, 2} の先手番局面（ply 9, 7）。
        let plies: Vec<u32> =
            mate_candidates(&moves, b, 4, 2).into_iter().map(|(ply, _)| ply).collect();
        assert_eq!(plies, vec![7, 9]);

        // tail 8, stride 4: d が {0, 4} の先手番局面（ply 9, 5）。
        let plies: Vec<u32> =
            mate_candidates(&moves, b, 8, 4).into_iter().map(|(ply, _)| ply).collect();
        assert_eq!(plies, vec![5, 9]);

        // tail が全体を覆う場合は全先手番局面。
        let plies: Vec<u32> =
            mate_candidates(&moves, b, 100, 2).into_iter().map(|(ply, _)| ply).collect();
        assert_eq!(plies, vec![1, 3, 5, 7, 9]);

        // 後手勝者なら後手番局面（d が奇数になるため stride 1 で全対象）。
        let plies: Vec<u32> =
            mate_candidates(&moves, w, 4, 1).into_iter().map(|(ply, _)| ply).collect();
        assert_eq!(plies, vec![6, 8]);

        assert!(mate_candidates(&[], b, 16, 2).is_empty());
    }

    #[test]
    fn oracle_finds_mate_in_one() {
        enable_material_eval();
        let oracle = oracle_search_mate(MATE_IN_ONE_SFEN, Color::Black, 7, None)
            .expect("oracle")
            .expect("mate expected");
        assert_eq!(oracle.mate_in, 1);
        assert_eq!(oracle.bestmove_usi, "G*5b");
    }

    #[test]
    fn oracle_finds_mate_in_three() {
        enable_material_eval();
        let oracle = oracle_search_mate(MATE_IN_THREE_SFEN, Color::Black, 9, None)
            .expect("oracle")
            .expect("mate expected");
        assert_eq!(oracle.mate_in, 3);
        assert_eq!(oracle.bestmove_usi, "G*5c");
    }

    #[test]
    fn oracle_nodes_limit_can_leave_synthetic_mate_unselected() {
        enable_material_eval();
        // fresh Search・単一スレッドでは node counter の進み方も決定的。この合成 3 手詰めは
        // 1 node では読み切れず不採用だが、同じ depth の無制限探索では採用される。
        assert_eq!(
            oracle_search_mate(MATE_IN_THREE_SFEN, Color::Black, 9, Some(1)).expect("limited"),
            None
        );
        assert!(
            oracle_search_mate(MATE_IN_THREE_SFEN, Color::Black, 9, None)
                .expect("unlimited")
                .is_some()
        );
    }

    #[test]
    fn declaration_form_is_not_mate_with_entering_king_rule_none() {
        enable_material_eval();
        // 前提: この局面は Point27 宣言が成立する形。
        let mut pos = Position::new();
        pos.set_sfen(DECLARATION_FORM_SFEN).expect("sfen");
        assert_eq!(pos.declaration_win(EnteringKingRule::Point27), Move::WIN);

        // EnteringKingRule 有効の探索は root 宣言勝ちを mate 帯（Value::MATE + Move::WIN）
        // で返す＝これが oracle で宣言判定を無効化する理由。
        let (score, best_move) = oracle_raw_search(
            DECLARATION_FORM_SFEN,
            Color::Black,
            7,
            None,
            EnteringKingRule::Point27,
        )
        .expect("search");
        assert_eq!(score, Value::MATE);
        assert_eq!(best_move, Move::WIN);
        // Value::MATE（mate_ply 0）は winner_mate_in の防御でも不採用になる。
        assert_eq!(winner_mate_in(score), None);

        // oracle（EnteringKingRule::None）では宣言が無効化され、mate 帯は出ない。
        let (score, best_move) =
            oracle_raw_search(DECLARATION_FORM_SFEN, Color::Black, 7, None, EnteringKingRule::None)
                .expect("search");
        assert!(!score.is_mate_score(), "宣言無効なら mate 帯は出ない: {}", score.raw());
        assert!(best_move.is_normal());
        assert_eq!(
            oracle_search_mate(DECLARATION_FORM_SFEN, Color::Black, 7, None).expect("oracle"),
            None
        );
    }

    /// 先手が 1 手詰めの局面で後手が投了する合成対局（`MATE_IN_ONE_SFEN` と同じ配置）。
    /// 最終手 G*5b（詰み）の局面 = 候補距離 0 が mate in 1。
    const TORYO_MATE_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+53FU59OU00KI\n",
        "P-51OU\n",
        "+\n",
        "+0052KI\nT1\n",
        "%TORYO\n",
    );

    /// 玉しかおらず詰みが存在しないまま後手が投了する合成対局（候補は出るが不採用）。
    const TORYO_NO_MATE_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-1918OU\nT1\n",
        "+5352OU\nT1\n",
        "%TORYO\n",
    );

    /// %TORYO だが途中の相手手が不正（Move::NONE fallback + 打ち切り）の対局。
    const TORYO_BROKEN_CSA: &str = concat!(
        "V2.2\n",
        "N+B\nN-W\n",
        "P+54OU\n",
        "P-19OU\n",
        "+\n",
        "+5453OU\nT1\n",
        "-5556FU\nT1\n",
        "+5352OU\nT1\n",
        "%TORYO\n",
    );

    fn build_mates_args(input: &Path, out_dir: &Path) -> BuildMatesArgs {
        BuildMatesArgs {
            input: input.to_path_buf(),
            out_dir: out_dir.to_path_buf(),
            tail_plies: 16,
            stride: 2,
            max_games: 0,
            oracle_depth: 7,
            oracle_nodes: None,
            threads: 1,
        }
    }

    #[test]
    fn run_build_mates_writes_mates_and_meta() {
        let in_dir = tempfile::tempdir().expect("tempdir");
        write_csa(in_dir.path(), "a_mate.csa", TORYO_MATE_CSA);
        write_csa(in_dir.path(), "b_no_mate.csa", TORYO_NO_MATE_CSA);
        write_csa(in_dir.path(), "c_broken.csa", TORYO_BROKEN_CSA);
        // %KACHI 対局は toryo に数えず対象外。
        write_csa(in_dir.path(), "d_kachi.csa", KACHI_ENTRY_CSA);
        let out_dir = tempfile::tempdir().expect("tempdir");

        run_build_mates(&build_mates_args(in_dir.path(), out_dir.path())).expect("run_build_mates");

        let mates_text =
            fs::read_to_string(out_dir.path().join("mates.jsonl")).expect("mates.jsonl");
        let mates: Vec<MateRecord> = mates_text
            .lines()
            .map(|l| serde_json::from_str(l).expect("mate json"))
            .collect();
        assert_eq!(mates.len(), 1);
        let record = &mates[0];
        assert_eq!(record.winner, 'b');
        assert_eq!(record.ply, 1);
        assert_eq!(record.mate_in, 1);
        assert_eq!(record.oracle_bestmove, "G*5b");
        assert_eq!(record.oracle_depth, 7);
        assert_eq!(record.oracle_nodes, None);
        assert_eq!(record.sfen, MATE_IN_ONE_SFEN);

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out_dir.path().join("meta.json")).unwrap())
                .expect("meta json");
        assert_eq!(meta["games_scanned"], 4);
        assert_eq!(meta["toryo_games"], 3);
        assert_eq!(meta["games_skipped_broken"], 1);
        assert_eq!(meta["games_skipped_winner_mismatch"], 0);
        // 詰み対局の候補 1 + 玉のみ対局の候補 2（ply 1, 3）。
        assert_eq!(meta["candidate_positions"], 3);
        assert_eq!(meta["mate_positions"], 1);
        assert_eq!(meta["games_with_mates"], 1);
        assert_eq!(meta["oracle_nodes"], serde_json::Value::Null);

        // 同一入力の再実行で mates.jsonl は byte 一致する（oracle の決定性）。
        let out_dir2 = tempfile::tempdir().expect("tempdir");
        run_build_mates(&build_mates_args(in_dir.path(), out_dir2.path()))
            .expect("run_build_mates twice");
        let mates_text2 =
            fs::read_to_string(out_dir2.path().join("mates.jsonl")).expect("mates.jsonl");
        assert_eq!(mates_text, mates_text2);
    }

    #[test]
    fn run_build_mates_max_games_limits_scan_order_prefix() {
        let in_dir = tempfile::tempdir().expect("tempdir");
        write_csa(in_dir.path(), "a_no_mate.csa", TORYO_NO_MATE_CSA);
        write_csa(in_dir.path(), "b_mate.csa", TORYO_MATE_CSA);
        let out_dir = tempfile::tempdir().expect("tempdir");

        let mut args = build_mates_args(in_dir.path(), out_dir.path());
        args.max_games = 1;
        run_build_mates(&args).expect("run_build_mates");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out_dir.path().join("meta.json")).unwrap())
                .expect("meta json");
        // 走査順（ファイル名ソート）の先頭 1 対局 = a_no_mate のみが走査される。
        assert_eq!(meta["games_scanned"], 1);
        assert_eq!(meta["toryo_games"], 1);
        assert_eq!(meta["mate_positions"], 0);
    }

    #[test]
    fn concordance_pairs_orders_by_mate_in_and_skips_equal() {
        // (mate_in, eval): mate_in 1 と 3 は順序一致、1 と 5 は tie、3 と 5 は逆転。
        let items = [(1, 500), (3, 300), (5, 500)];
        let pairs = concordance_pairs(&items);
        assert_eq!(pairs, vec![(0, 1, 1.0), (0, 2, 0.5), (1, 2, 0.0)]);

        // mate_in が同じ pair は数えない。
        let items = [(3, 100), (3, 200)];
        assert!(concordance_pairs(&items).is_empty());

        // near/far は mate_in の大小で決まる（並び順に依存しない）。
        let items = [(5, 100), (1, 200)];
        assert_eq!(concordance_pairs(&items), vec![(1, 0, 1.0)]);

        assert!(concordance_pairs(&[]).is_empty());
        assert!(concordance_pairs(&[(1, 100)]).is_empty());
    }

    #[test]
    fn top1_score_classifies_strict_max_tie_and_loss() {
        // 厳密最大。
        assert_eq!(top1_score(300, 300, 1), 1.0);
        // 最大タイに含まれる。
        assert_eq!(top1_score(300, 300, 2), 0.5);
        // 最大でない。
        assert_eq!(top1_score(100, 300, 1), 0.0);
        assert_eq!(top1_score(100, 300, 3), 0.0);
    }

    fn write_mate_jsonl(path: &Path, records: &[MateRecord]) {
        let mut out = String::new();
        for record in records {
            out.push_str(&serde_json::to_string(record).expect("json"));
            out.push('\n');
        }
        fs::write(path, out).expect("write");
    }

    fn mate_record(source_csa: &str, ply: u32, mate_in: u32) -> MateRecord {
        MateRecord {
            source_csa: source_csa.to_string(),
            winner: 'b',
            ply,
            sfen: MATE_IN_ONE_SFEN.to_string(),
            mate_in,
            oracle_bestmove: "G*5b".to_string(),
            oracle_depth: 7,
            oracle_nodes: None,
        }
    }

    #[test]
    fn count_mate_clusters_distinguishes_concordance_and_top1() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mates.jsonl");
        write_mate_jsonl(
            &path,
            &[
                // 対局 a: mate_in が異なる 2 局面 → concordance / top-1 両方のクラスタ。
                mate_record("a.csa", 5, 3),
                mate_record("a.csa", 7, 1),
                // 対局 b: mate_in が同一 → top-1 のみ。
                mate_record("b.csa", 5, 3),
                mate_record("b.csa", 7, 3),
                // 対局 c: 1 局面のみ → top-1 のみ。
                mate_record("c.csa", 9, 1),
            ],
        );
        let counts = count_mate_clusters(&path).expect("counts");
        assert_eq!(
            counts,
            MateClusterCounts {
                concordance_games: 1,
                top1_games: 3,
            }
        );

        // source_csa の順序違反はエラー。
        let bad = dir.path().join("bad.jsonl");
        write_mate_jsonl(&bad, &[mate_record("b.csa", 5, 3), mate_record("a.csa", 7, 1)]);
        assert!(count_mate_clusters(&bad).is_err());
    }

    #[test]
    fn aggregator_ci_is_deterministic_end_to_end() {
        let build = || {
            let mut agg = PairAggregator::new([3, 3, 0, 0, 0], 500, DEFAULT_SEED);
            for agreement in [1.0, 0.0, 0.5] {
                let mut game = [GameAgg::default(); SLOTS];
                game[0] = GameAgg {
                    sum: agreement,
                    count: 1,
                };
                game[Condition::PointGain.index() + 1] = GameAgg {
                    sum: agreement,
                    count: 1,
                };
                agg.push_game(game).unwrap();
            }
            agg.finish().unwrap()
        };
        let (o1, c1) = build();
        let (o2, c2) = build();
        assert_eq!(o1, o2);
        assert_eq!(c1, c2);
        assert!(c1["point_gain"].ci95_lo.is_some());
    }
}
