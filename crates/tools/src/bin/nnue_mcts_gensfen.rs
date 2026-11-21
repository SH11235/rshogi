use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use rshogi_core::nnue::{get_network, init_nnue, set_fv_scale_override};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use rshogi_dl::Evaluator;
use rshogi_mcts::{Config, History, Limits, Outcome, Search};
use tempfile::NamedTempFile;
use tools::nnue_routing::{
    NativeRouting, ProgressCoefficients, parse_native_layer_stack_bucket_mode,
};
use tools::packed_sfen::{move_to_hcpe_move16, pack_position_hcp};
use tools::qsearch_pv::{Evaluator as StaticEvaluator, NnueEvaluator};

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
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i32).range(0..))]
    fv_scale: i32,
    #[arg(long)]
    ls_bucket_mode: Option<String>,
    #[arg(long)]
    ls_progress_buckets: Option<usize>,
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,
}

struct LeafEvaluator {
    nnue: NnueEvaluator,
    scale: f64,
}

impl Evaluator for LeafEvaluator {
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        logits.fill(0.0);
        let score = f64::from(self.nnue.evaluate(position));
        Ok((1.0 / (1.0 + (-score / self.scale).exp())) as f32)
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
            let search = Search::new(Config {
                max_game_ply: cli.max_moves,
                tree_memory_mb: cli.tree_memory_mb as usize,
                ..Config::default()
            })?;
            let nnue = NnueEvaluator::new().context("NNUE evaluator is unavailable")?;
            Ok((
                search,
                LeafEvaluator {
                    nnue,
                    scale: cli.value_scale,
                },
            ))
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
    use tools::hcpe3::{read_candidate, read_game_header, read_move_header};

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
}
