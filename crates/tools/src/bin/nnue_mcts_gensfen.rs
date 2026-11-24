use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use rshogi_core::movegen::{MAX_MOVES, MoveList, generate_legal_all};
use rshogi_core::nnue::{get_network, init_nnue, set_fv_scale_override};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move, Piece, Square, Value};
use rshogi_dl::Evaluator;
use rshogi_dl::features::make_move_label;
use rshogi_mcts::{Config, History, Limits, Outcome, Search};
use tempfile::NamedTempFile;
use tools::nnue_routing::{
    NativeRouting, ProgressCoefficients, parse_native_layer_stack_bucket_mode,
};
use tools::packed_sfen::{move_to_hcpe_move16, pack_position_hcp};
use tools::qsearch_pv::{Evaluator as StaticEvaluator, NnueEvaluator, NnueStacks, qsearch_nnue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Prior {
    Uniform,
    Static,
    Qsearch,
}

/// NNUE を葉評価に使う MCTS で平手を自己対局し、HCPE3 教師を作る。
#[derive(Parser)]
struct Cli {
    #[arg(long)]
    eval_file: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..))]
    games: u32,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    workers: u32,
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u64).range(2..=65535))]
    visits: u64,
    /// worker ごとの探索木のメモリ上限（MiB）。
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=1024))]
    tree_memory_mb: u32,
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(i32).range(1..=512))]
    max_moves: i32,
    /// 序盤のこの手数まで、訪問数に比例して着手をサンプルする。
    #[arg(long, default_value_t = 24)]
    sample_plies: u16,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// NNUE スコアを期待得点に変換する sigmoid の尺度。
    #[arg(long, default_value_t = 600.0)]
    value_scale: f64,
    /// 合法手の事前確率を求める方法。
    #[arg(long, value_enum, default_value = "uniform")]
    prior: Prior,
    /// 子局面の評価を事前確率へ変換する softmax の温度（cp）。
    #[arg(long, default_value_t = 100.0, value_parser = parse_prior_temperature)]
    prior_temperature: f64,
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i32).range(0..))]
    fv_scale: i32,
    #[arg(long)]
    ls_bucket_mode: Option<String>,
    #[arg(long)]
    ls_progress_buckets: Option<usize>,
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,
}

fn parse_prior_temperature(text: &str) -> Result<f64> {
    let value: f64 = text.parse()?;
    ensure!(value.is_finite() && value > 0.0, "temperature must be positive and finite");
    Ok(value)
}

const PRIOR_QSEARCH_PLIES: i32 = 8;

struct LeafEvaluator {
    nnue: NnueEvaluator,
    scale: f64,
    prior: Option<ValuePrior>,
}

impl LeafEvaluator {
    fn new(cli: &Cli, config: &Config) -> Result<Self> {
        let nnue = NnueEvaluator::new().context("NNUE evaluator is unavailable")?;
        let prior = if cli.prior == Prior::Uniform {
            None
        } else {
            let mut scratch = Position::new();
            scratch.try_reserve_move_history((1 + PRIOR_QSEARCH_PLIES) as usize)?;
            Some(ValuePrior {
                scratch,
                stacks: NnueStacks::new(),
                use_qsearch: cli.prior == Prior::Qsearch,
                temperature: cli.prior_temperature,
                max_game_ply: config.max_game_ply,
                entering_king_rule: config.entering_king_rule,
            })
        };
        Ok(Self {
            nnue,
            scale: cli.value_scale,
            prior,
        })
    }
}

impl Evaluator for LeafEvaluator {
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        logits.fill(0.0);
        let score = f64::from(match &mut self.prior {
            Some(prior) => prior.evaluate(position, logits)?,
            None => self.nnue.evaluate(position),
        });
        Ok((1.0 / (1.0 + (-score / self.scale).exp())) as f32)
    }
}

struct ValuePrior {
    scratch: Position,
    stacks: NnueStacks,
    use_qsearch: bool,
    temperature: f64,
    max_game_ply: i32,
    entering_king_rule: EnteringKingRule,
}

impl ValuePrior {
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<i32> {
        let side = position.side_to_move();
        let mut board = [Piece::NONE; Square::NUM];
        for sq in position.occupied().iter() {
            board[sq.index()] = position.piece_on(sq);
        }
        // 探索から借りた局面は読み取り専用なので、確保済みの作業領域で子を指す。
        self.scratch.set_from_parts(
            &board,
            &[position.hand(Color::Black), position.hand(Color::White)],
            side,
        )?;
        self.stacks.reset();
        let leaf_score = self.stacks.evaluate(&self.scratch);
        let mut legal = MoveList::new();
        generate_legal_all(position, &mut legal);
        let mut scores = [0; MAX_MOVES];
        let scores = &mut scores[..legal.len()];
        for (&mv, score) in legal.iter().zip(scores.iter_mut()) {
            let dirty = self.scratch.do_move(mv, self.scratch.gives_check(mv));
            self.stacks.push(dirty);
            let outcome =
                child_outcome(position, &self.scratch, self.entering_king_rule, self.max_game_ply);
            *score = prior_score_cp(outcome, || {
                if self.use_qsearch {
                    qsearch_nnue(&mut self.scratch, &mut self.stacks, PRIOR_QSEARCH_PLIES)
                } else {
                    self.stacks.evaluate(&self.scratch)
                }
            });
            self.stacks.pop();
            self.scratch.undo_move(mv);
        }
        // 極小の温度では最大値を引き、評価の異なる手が同じ上限に飽和するのを防ぐ。
        let limit = f64::from(f32::MAX) / 2.0;
        let max_abs = scores.iter().map(|score| score.unsigned_abs()).max().unwrap_or(0);
        let offset = if f64::from(max_abs) / self.temperature > limit {
            f64::from(scores.iter().copied().max().unwrap_or(0))
        } else {
            0.0
        };
        for (&mv, &score) in legal.iter().zip(scores.iter()) {
            let logit = (f64::from(score) - offset) / self.temperature;
            logits[make_move_label(mv, side)] = logit.max(-limit) as f32;
        }
        Ok(leaf_score)
    }
}

fn prior_score_cp(child_outcome: Option<Outcome>, evaluate_child: impl FnOnce() -> i32) -> i32 {
    let mate = Value::MATE.to_cp();
    match child_outcome {
        Some(Outcome::Win) => -mate,
        Some(Outcome::Loss) => mate,
        Some(Outcome::Draw) => 0,
        // 通常評価も cp 変換で詰みの値を超え得るため、変換後に終局値の内側へ収める。
        None => -Value::new(evaluate_child()).to_cp().clamp(1 - mate, mate - 1),
    }
}

fn child_outcome(
    parent: &Position,
    child: &Position,
    entering_king_rule: EnteringKingRule,
    max_game_ply: i32,
) -> Option<Outcome> {
    if let Some(outcome) = child_repetition(parent, child) {
        return Some(outcome);
    }
    if child.declaration_win(entering_king_rule) != Move::NONE {
        return Some(Outcome::Win);
    }
    let mut replies = MoveList::new();
    generate_legal_all(child, &mut replies);
    if replies.is_empty() {
        return Some(Outcome::Loss);
    }
    (parent.game_ply() >= max_game_ply).then_some(Outcome::Draw)
}

fn child_repetition(parent: &Position, child: &Position) -> Option<Outcome> {
    // Position 自体の千日手判定は直近 16 手に限られる。MCTS は対局開始まで遡る。
    let key = child.key();
    let fourth = (0..parent.state_index())
        .rev()
        .step_by(2)
        .filter(|&index| parent.state_at(index).key() == key)
        .nth(2)?;
    let distance = (parent.state_index() + 1 - fourth) as i32;
    let side = child.side_to_move();
    let checks = parent.state().continuous_check;
    if checks[side.index()] >= distance {
        Some(Outcome::Loss)
    } else if child.in_check() && checks[(!side).index()] + 2 >= distance {
        Some(Outcome::Win)
    } else {
        Some(Outcome::Draw)
    }
}

fn load_nnue(cli: &Cli) -> Result<()> {
    ensure!(cli.value_scale.is_finite() && cli.value_scale > 0.0, "invalid --value-scale");
    set_fv_scale_override(cli.fv_scale);
    init_nnue(&cli.eval_file)
        .map_err(anyhow::Error::msg)
        .context("NNUE load failed")?;
    let net = get_network().context("NNUE is unavailable")?;
    let mode = cli
        .ls_bucket_mode
        .as_deref()
        .map(parse_native_layer_stack_bucket_mode)
        .transpose()?;
    if let Some(routing) = NativeRouting::prepare(
        net.layer_stack_num_buckets(),
        mode,
        cli.ls_progress_buckets,
        cli.ls_progress_coeff.is_some(),
    )? {
        routing.apply(|| {
            cli.ls_progress_coeff.as_deref().map(ProgressCoefficients::load).transpose()
        })?;
    }
    Ok(())
}

struct Game {
    start: [u8; 32],
    moves: Vec<Ply>,
    result: u8,
}

struct Ply {
    played: Move,
    score: i16,
    policy: Vec<(Move, u64)>,
}

impl Game {
    fn write(&self, output: &mut impl Write, max_moves: i32) -> Result<()> {
        let game_info = match max_moves {
            256 => 4,
            320 => 8,
            512 => 12,
            _ => 0,
        };
        output.write_all(&self.start)?;
        output.write_all(&u16::try_from(self.moves.len())?.to_le_bytes())?;
        output.write_all(&[self.result, game_info])?;
        for ply in &self.moves {
            output.write_all(&move_to_hcpe_move16(ply.played).to_le_bytes())?;
            output.write_all(&ply.score.to_le_bytes())?;
            output.write_all(&u16::try_from(ply.policy.len())?.to_le_bytes())?;
            for &(mv, visits) in &ply.policy {
                output.write_all(&move_to_hcpe_move16(mv).to_le_bytes())?;
                output.write_all(&u16::try_from(visits)?.to_le_bytes())?;
            }
        }
        Ok(())
    }
}

fn sample_move(policy: &[(Move, u64)], rng: &mut ChaCha8Rng) -> Option<Move> {
    let total = policy.iter().map(|(_, visits)| visits).sum::<u64>();
    let mut ticket = rng.random_range(0..total);
    for &(mv, visits) in policy {
        if ticket < visits {
            return Some(mv);
        }
        ticket -= visits;
    }
    None
}

fn play(search: &mut Search, evaluator: &mut impl Evaluator, cli: &Cli, id: u32) -> Result<Game> {
    let mut position = Position::new();
    position.set_hirate();
    let mut history = History::new(&position);
    let mut rng = ChaCha8Rng::seed_from_u64(cli.seed.wrapping_add(u64::from(id)));
    let mut game = Game {
        start: pack_position_hcp(&position),
        moves: Vec::with_capacity(cli.max_moves as usize),
        result: 0,
    };
    let stop = AtomicBool::new(false);
    loop {
        let result = search.run(
            &mut position,
            &mut history,
            evaluator,
            Limits {
                visits: cli.visits,
                time: None,
            },
            &stop,
        )?;
        if let Some(outcome) = result.outcome.filter(|_| result.visits == 0) {
            let stm = position.side_to_move();
            game.result = match outcome {
                Outcome::Win if stm == Color::Black => 1,
                Outcome::Loss if stm == Color::White => 1,
                Outcome::Win | Outcome::Loss => 2,
                Outcome::Draw => 0,
            };
            game.result |= if history.outcome().is_some() {
                4
            } else if result.best_move == Move::WIN {
                8
            } else if outcome == Outcome::Draw {
                16
            } else {
                0
            };
            return Ok(game);
        }
        ensure!(
            !result.memory_full,
            "MCTS tree filled at ply {} before the visit limit. Increase --tree-memory-mb",
            position.game_ply()
        );
        let policy = search.root_move_visits().collect::<Vec<_>>();
        ensure!(!policy.is_empty(), "MCTS recorded no root move visits");
        let played = if result.outcome.is_none() && game.moves.len() < usize::from(cli.sample_plies)
        {
            sample_move(&policy, &mut rng).context("no move sampled")?
        } else {
            result.best_move
        };
        let value = f64::from(result.value);
        let score = (cli.value_scale * (value / (1.0 - value)).ln()).clamp(-10000.0, 10000.0);
        game.moves.push(Ply {
            played,
            score: score.round() as i16,
            policy,
        });
        position.do_move(played, position.gives_check(played));
        history.push(&position);
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure!(!cli.output.try_exists()?, "output already exists");
    load_nnue(&cli)?;
    let count = cli.games.min(cli.workers) as usize;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(count).build()?;
    let mut workers = (0..count)
        .map(|_| {
            let config = Config {
                max_game_ply: cli.max_moves,
                tree_memory_mb: cli.tree_memory_mb as usize,
                ..Config::default()
            };
            let evaluator = LeafEvaluator::new(&cli, &config)?;
            Ok((Search::new(config)?, evaluator))
        })
        .collect::<Result<Vec<_>>>()?;
    let parent = cli
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = NamedTempFile::new_in(parent)?;
    let mut rows = 0;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        for first in (0..cli.games).step_by(count) {
            let batch_size = (cli.games - first).min(count as u32) as usize;
            let games = pool.install(|| {
                workers[..batch_size]
                    .par_iter_mut()
                    .enumerate()
                    .map(|(offset, (search, evaluator))| {
                        let id = first + offset as u32;
                        play(search, evaluator, &cli, id)
                            .with_context(|| format!("self-play game {}", id + 1))
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            for game in games {
                game.write(&mut writer, cli.max_moves)?;
                rows += game.moves.len() as u64;
            }
        }
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(&cli.output)?;
    eprintln!("games={} positions={rows}", cli.games);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::eval::{MaterialLevel, set_material_level};
    use rshogi_dl::features::MAX_MOVE_LABEL_NUM;
    use std::sync::Mutex;
    use tools::hcpe3::{read_candidate, read_game_header, read_move_header};

    static EVAL_LOCK: Mutex<()> = Mutex::new(());

    struct ConstantEvaluator;

    impl Evaluator for ConstantEvaluator {
        fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
            logits.fill(0.0);
            Ok(0.5)
        }
    }

    #[test]
    fn fixed_seed_writes_repeatable_hcpe3_with_raw_visits() -> Result<()> {
        let cli = Cli::try_parse_from([
            "nnue_mcts_gensfen",
            "--eval-file",
            "nn.bin",
            "--output",
            "games.hcpe3",
            "--visits",
            "40",
            "--max-moves",
            "4",
        ])?;
        assert_eq!(cli.prior, Prior::Uniform);
        let mut search = Search::new(Config {
            max_game_ply: cli.max_moves,
            ..Config::default()
        })?;
        let mut first = Vec::new();
        let mut second = Vec::new();
        play(&mut search, &mut ConstantEvaluator, &cli, 0)?.write(&mut first, cli.max_moves)?;
        play(&mut search, &mut ConstantEvaluator, &cli, 0)?.write(&mut second, cli.max_moves)?;
        assert_eq!(first, second);
        assert_eq!(first[34], 16);
        let mut bytes = first.as_slice();
        let header = read_game_header(&mut bytes)?.context("missing game")?;
        assert_eq!(header.move_count, 4);
        assert_eq!(header.result, 0);
        for index in 0..header.move_count {
            let ply = read_move_header(&mut bytes)?;
            let candidates = (0..ply.candidate_count)
                .map(|_| read_candidate(&mut bytes))
                .collect::<std::io::Result<Vec<_>>>()?;
            assert!(candidates.len() > 1);
            assert!(candidates.iter().any(|&(mv, count)| mv == ply.move16 && count > 0));
            let visits = candidates.iter().map(|&(_, n)| u64::from(n)).sum::<u64>();
            if index + 1 == header.move_count {
                assert!(visits <= 39);
            } else {
                assert_eq!(visits, 39);
            }
        }
        assert!(read_game_header(&mut bytes)?.is_none());
        Ok(())
    }

    fn static_cli() -> Result<Cli> {
        Ok(Cli::try_parse_from([
            "nnue_mcts_gensfen",
            "--eval-file",
            "nn.bin",
            "--output",
            "games.hcpe3",
            "--visits",
            "40",
            "--max-moves",
            "4",
            "--prior",
            "static",
        ])?)
    }

    #[test]
    fn static_prior_writes_repeatable_hcpe3() -> Result<()> {
        let _eval_guard = EVAL_LOCK.lock().unwrap();
        set_material_level(MaterialLevel::Lv1);
        let cli = static_cli()?;
        let config = Config {
            max_game_ply: cli.max_moves,
            ..Config::default()
        };
        let mut evaluator = LeafEvaluator::new(&cli, &config)?;
        let mut search = Search::new(config)?;
        let mut first = Vec::new();
        let mut second = Vec::new();
        play(&mut search, &mut evaluator, &cli, 0)?.write(&mut first, cli.max_moves)?;
        play(&mut search, &mut evaluator, &cli, 0)?.write(&mut second, cli.max_moves)?;
        assert_eq!(first, second);
        let header = read_game_header(&mut first.as_slice())?.context("missing game")?;
        assert_eq!(header.move_count, 4);
        Ok(())
    }

    #[test]
    fn static_prior_ranks_free_capture_first() -> Result<()> {
        let _eval_guard = EVAL_LOCK.lock().unwrap();
        set_material_level(MaterialLevel::Lv1);
        let mut position = Position::new();
        position.set_sfen("4k4/9/9/4r4/4G4/9/9/9/4K4 b - 1")?;
        let capture = position.to_move(Move::from_usi("5e5d").unwrap()).unwrap();
        let mut legal = MoveList::new();
        generate_legal_all(&position, &mut legal);
        assert!(legal.contains(capture));

        let mut cli = static_cli()?;
        for temperature in [cli.prior_temperature, f64::MIN_POSITIVE] {
            cli.prior_temperature = temperature;
            let mut evaluator = LeafEvaluator::new(&cli, &Config::default())?;
            let mut logits = [f32::NAN; MAX_MOVE_LABEL_NUM];
            let value = evaluator.evaluate(&position, &mut logits)?;
            let uniform = LeafEvaluator::new(
                &Cli {
                    prior: Prior::Uniform,
                    ..static_cli()?
                },
                &Config::default(),
            )?
            .evaluate(&position, &mut [0.0; MAX_MOVE_LABEL_NUM])?;
            assert_eq!(value.to_bits(), uniform.to_bits());
            assert!(logits.iter().all(|x| x.is_finite()));
            let label = |mv| make_move_label(mv, position.side_to_move());
            assert!(
                legal
                    .iter()
                    .filter(|&&mv| mv != capture)
                    .all(|&mv| logits[label(mv)] < logits[label(capture)])
            );
        }
        Ok(())
    }

    #[test]
    fn terminal_prior_scores_bound_nonterminal_cp() {
        let win = prior_score_cp(Some(Outcome::Loss), || unreachable!());
        let loss = prior_score_cp(Some(Outcome::Win), || unreachable!());
        let strong = prior_score_cp(None, || -30_000);
        let weak = prior_score_cp(None, || 30_000);

        assert_eq!(win, Value::MATE.to_cp());
        assert_eq!(loss, -Value::MATE.to_cp());
        assert_eq!(strong, win - 1);
        assert_eq!(weak, loss + 1);
    }

    #[cfg(feature = "nnue-arch")]
    #[test]
    fn nnue_child_matches_full_evaluation_and_undo_restores_parent() -> Result<()> {
        use rshogi_core::eval::disable_material;
        use rshogi_core::nnue::net_delta::test_utils::build_synthetic_layer_stacks;
        use rshogi_core::nnue::{
            HALFKA_HM_DIMENSIONS, LayerStackBucketMode, clear_nnue, configure_layer_stack_routing,
            init_nnue_from_bytes,
        };

        let _eval_guard = EVAL_LOCK.lock().unwrap();
        disable_material();
        const L1: usize = 64;
        let mut net =
            build_synthetic_layer_stacks("HalfKaHmMerged", HALFKA_HM_DIMENSIONS, L1, 16, 32, 1);
        let ft_start = net.ft_biases + L1;
        let ft_end = ft_start + HALFKA_HM_DIMENSIONS * L1;
        let (rows, _) = net.bytes[ft_start..ft_end].as_chunks_mut::<L1>();
        // 一手で特徴インデックスが変わると評価にも反映されるよう、行ごとに重みを変える。
        for (index, row) in rows.iter_mut().enumerate() {
            row.fill((index % 13 + 1) as u8);
        }
        init_nnue_from_bytes(&net.bytes)?;
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, 1, Some(1))
            .map_err(anyhow::Error::msg)?;

        let mut position = Position::new();
        position.set_sfen("4k4/9/9/4r4/4G4/9/9/9/4K4 b - 1")?;
        let mut stacks = NnueStacks::new();
        let full = NnueEvaluator::new().context("NNUE evaluator is unavailable")?;
        let parent = stacks.evaluate(&position);
        assert_eq!(parent, full.evaluate(&position));
        let mut children = [0; 2];
        for (usi, score) in ["5e5d", "5e4e"].into_iter().zip(&mut children) {
            let mv = position.to_move(Move::from_usi(usi).unwrap()).unwrap();
            let dirty = position.do_move(mv, position.gives_check(mv));
            stacks.push(dirty);
            let child = stacks.evaluate(&position);
            assert_eq!(child, full.evaluate(&position), "{usi}");
            *score = child;
            stacks.pop();
            position.undo_move(mv);
            assert_eq!(stacks.evaluate(&position), parent, "{usi}");
        }
        assert_ne!(children[0], children[1]);
        clear_nnue();
        Ok(())
    }

    #[test]
    fn child_repetition_matches_mcts_beyond_sixteen_plies() -> Result<()> {
        let mut position = Position::new();
        position.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
        let mut history = History::new(&position);
        let cycle = [
            "5i6i", "5a6a", "6i7i", "6a7a", "7i8i", "7a8a", "8i9i", "8a9a", "9i9h", "9a9b", "9h8h",
            "9b8b", "8h7h", "8b7b", "7h6h", "7b6b", "6h5h", "6b5b", "5h5i", "5b5a",
        ];
        for usi in cycle.into_iter().cycle().take(3 * cycle.len() - 1) {
            let mv = position.to_move(Move::from_usi(usi).unwrap()).unwrap();
            position.do_move(mv, position.gives_check(mv));
            history.push(&position);
        }
        let mut child = position.clone();
        let mv = child.to_move(Move::from_usi("5b5a").unwrap()).unwrap();
        child.do_move(mv, child.gives_check(mv));
        history.push(&child);
        assert_eq!(history.outcome(), Some(Outcome::Draw));
        assert_eq!(child_repetition(&position, &child), history.outcome());
        Ok(())
    }

    #[test]
    fn qsearch_prior_accounts_for_recapture() -> Result<()> {
        let _eval_guard = EVAL_LOCK.lock().unwrap();
        set_material_level(MaterialLevel::Lv1);
        let mut position = Position::new();
        position.set_sfen("4k4/9/5g3/4r4/4G4/9/9/9/4K4 b - 1")?;
        let capture = position.to_move(Move::from_usi("5e5d").unwrap()).unwrap();
        let label = make_move_label(capture, position.side_to_move());
        let config = Config::default();
        let mut cli = static_cli()?;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        let static_value = LeafEvaluator::new(&cli, &config)?.evaluate(&position, &mut logits)?;
        let capture_logit = logits[label];
        cli.prior = Prior::Qsearch;
        let qsearch_value = LeafEvaluator::new(&cli, &config)?.evaluate(&position, &mut logits)?;
        assert_eq!(static_value.to_bits(), qsearch_value.to_bits());
        assert!(logits[label] < capture_logit);
        Ok(())
    }
}
