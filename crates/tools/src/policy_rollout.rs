//! dlshogi 系 policy で種の局面から指し進め、途中の局面を PSV に書き出す処理 (`policy_rollout_psv`)。
//!
//! 1 手ごとに policy を 1 回推論し、合法手の確率から温度付きで抽選する (探索はしない)。
//! 多数の指し進めを同時に走らせ、全局面を 1 回の batch 推論にまとめる。同じ推論の
//! value head を各局面の score (手番視点 cp) にする。推論器は [`PolicyValueModel`] で差し替えられる。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::ValueEnum;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;

use rshogi_core::movegen::{MoveList, generate_legal};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move, RepetitionState};

use crate::dlshogi_features::{MAX_MOVE_LABEL_NUM, make_move_label};
use crate::output_path::create_safe_output_file;
use crate::packed_sfen::{
    PackedSfenValue, is_legal_psv_move, move_to_psv_move16, pack_position, psv_move16_to_move,
    same_psv_move, unpack_sfen,
};

/// 出力 PSV の game_result の決め方
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GameResultMode {
    /// 常に 0
    Zero,
    /// 指し進めの結末 (詰み・宣言・連続王手の千日手は勝敗、千日手・手数上限は 0)
    Rollout,
}

/// 抽選と打ち切りの設定
#[derive(Clone, Debug)]
pub struct RolloutConfig {
    /// 1 つの種から始める指し進めの本数
    pub rollouts_per_seed: u32,
    /// 同時に進める指し進めの数 (= 1 回の推論の最大 batch)
    pub parallel: usize,
    /// 抽選の温度。確率 p の手を p^(1/T) で重み付けする。0 で常に最善手
    pub temperature: f32,
    /// 合法手で正規化した確率がこれ未満の手を抽選から外す (最善手は残す)
    pub min_prob: f32,
    /// 確率の上位 k 手だけから抽選する (0 で制限なし)
    pub top_k: usize,
    /// 通し手数がこの値に達したら打ち切る
    pub max_game_ply: u16,
    /// 出力の game_result に入れる値
    pub game_result: GameResultMode,
    /// 乱数の seed
    pub seed: u64,
    /// 種からの最初の 1 手で、種のレコードの move16 (元の対局で指された手) を選ばない
    pub avoid_seed_move: bool,
}

/// 局面の batch から value (手番視点 cp) と policy logit を返す推論器
pub trait PolicyValueModel {
    /// `values` に局面ごとの cp、`policy` に局面ごとの `MAX_MOVE_LABEL_NUM` 個の logit を追記する。
    fn infer(
        &mut self,
        positions: &[&Position],
        values: &mut Vec<i32>,
        policy: &mut Vec<f32>,
    ) -> Result<()>;
}

#[cfg(feature = "dlshogi-onnx")]
impl PolicyValueModel for crate::onnx_value::OnnxValueEvaluator {
    fn infer(
        &mut self,
        positions: &[&Position],
        values: &mut Vec<i32>,
        policy: &mut Vec<f32>,
    ) -> Result<()> {
        self.evaluate_with_policy(positions, values, policy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndReason {
    Mated,
    Declaration,
    Repetition,
    PerpetualCheck,
    MaxPly,
}

/// 結末。勝者 (無ければ引き分け扱い) と終わり方。
#[derive(Clone, Copy, Debug)]
struct Ending {
    winner: Option<Color>,
    reason: EndReason,
}

struct Game {
    pos: Position,
    rng: ChaCha8Rng,
    game_ply: u16,
    moves_from_seed: u32,
    /// 種の局面も書き出す (同じ種の指し進めのうち 1 本だけが持つ)
    emit_seed: bool,
    /// 種からの最初の 1 手で選ばない手
    avoid_first: Option<Move>,
    legal: MoveList,
    ending: Option<Ending>,
    records: Vec<(PackedSfenValue, Color)>,
}

/// 入力ファイルの識別子・種の通し番号・指し進めの番号から、その指し進め専用の乱数列を作る。
///
/// batch の組み方や chunk の区切り、入力ファイルを渡す順に依らず、各指し進めの手順が同じになる。
fn rollout_rng(seed: u64, stream: u64, seed_index: u64, rollout_index: u32) -> ChaCha8Rng {
    fn splitmix64(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
    let h = splitmix64(
        splitmix64(splitmix64(seed ^ splitmix64(stream)) ^ seed_index) ^ u64::from(rollout_index),
    );
    ChaCha8Rng::seed_from_u64(h)
}

impl Game {
    /// 現局面で指し進めが終わるかを判定し、続くなら合法手を `legal` に残す。
    fn update_status(&mut self, max_game_ply: u16) {
        let stm = self.pos.side_to_move();
        if self.moves_from_seed > 0 {
            // 種より前の履歴は持たないので、千日手は指し進めの中の同一局面 4 回だけを見る。
            match self.pos.repetition_state(0) {
                RepetitionState::Draw => {
                    self.ending = Some(Ending {
                        winner: None,
                        reason: EndReason::Repetition,
                    });
                    return;
                }
                RepetitionState::Win => {
                    self.ending = Some(Ending {
                        winner: Some(stm),
                        reason: EndReason::PerpetualCheck,
                    });
                    return;
                }
                RepetitionState::Lose => {
                    self.ending = Some(Ending {
                        winner: Some(!stm),
                        reason: EndReason::PerpetualCheck,
                    });
                    return;
                }
                _ => {}
            }
        }
        self.legal = MoveList::new();
        generate_legal(&self.pos, &mut self.legal);
        if self.legal.is_empty() {
            self.ending = Some(Ending {
                winner: Some(!stm),
                reason: EndReason::Mated,
            });
        } else if self.pos.declaration_win(EnteringKingRule::Point27) != Move::NONE {
            self.ending = Some(Ending {
                winner: Some(stm),
                reason: EndReason::Declaration,
            });
        } else if self.game_ply >= max_game_ply {
            self.ending = Some(Ending {
                winner: None,
                reason: EndReason::MaxPly,
            });
        }
    }
}

/// 合法手の logit から 1 手を抽選し、`logits` 内の添字を返す。
///
/// `excluded` の手は合法手から除いて扱う (残りの手が 1 つ以上あること)。
/// 確率は残りの合法手だけで softmax した値。`min_prob` 未満を除き (最善手は残す)、上位 `top_k` に
/// 絞ってから p^(1/T) で抽選する。`scratch` は呼び出し間で再利用する作業領域。
fn choose_move(
    logits: &[f32],
    excluded: Option<usize>,
    cfg: &RolloutConfig,
    rng: &mut ChaCha8Rng,
    scratch: &mut Vec<(usize, f32)>,
) -> usize {
    debug_assert!(logits.len() > usize::from(excluded.is_some()));
    let allowed = |i: &usize| Some(*i) != excluded;
    let mut best = (0..logits.len()).find(allowed).unwrap_or(0);
    for i in (0..logits.len()).filter(allowed) {
        if logits[i] > logits[best] {
            best = i;
        }
    }
    if cfg.temperature == 0.0 {
        return best;
    }
    let max = logits[best];
    let denom: f64 = (0..logits.len())
        .filter(allowed)
        .map(|i| f64::from(logits[i] - max).exp())
        .sum();

    scratch.clear();
    for (i, &l) in logits.iter().enumerate().filter(|(i, _)| allowed(i)) {
        let p = f64::from(l - max).exp() / denom;
        if i == best || p >= f64::from(cfg.min_prob) {
            scratch.push((i, l));
        }
    }
    if cfg.top_k > 0 && scratch.len() > cfg.top_k {
        // 同じ logit は添字の小さい手を優先し、出力を決定的にする。
        scratch.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scratch.truncate(cfg.top_k);
    }

    let inv_t = 1.0 / f64::from(cfg.temperature);
    let total: f64 = scratch.iter().map(|&(_, l)| (f64::from(l - max) * inv_t).exp()).sum();
    let mut r = rng.random::<f64>() * total;
    for &(i, l) in scratch.iter() {
        r -= (f64::from(l - max) * inv_t).exp();
        if r < 0.0 {
            return i;
        }
    }
    scratch.last().map_or(best, |&(i, _)| i)
}

/// 1 chunk の集計
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ChunkStats {
    /// 終わった指し進めの数
    pub rollouts: u64,
    /// 書き出した局面数
    pub positions: u64,
    /// 局面として読めなかった種の数
    pub invalid_seeds: u64,
    /// 推論 (batch) の回数
    pub inferences: u64,
    /// 詰みで終わった数
    pub mated: u64,
    /// 入玉宣言で終わった数
    pub declaration: u64,
    /// 千日手で終わった数
    pub repetition: u64,
    /// 連続王手の千日手で終わった数
    pub perpetual_check: u64,
    /// 手数の上限で終わった数
    pub max_ply: u64,
}

impl ChunkStats {
    fn add(&mut self, o: &ChunkStats) {
        self.rollouts += o.rollouts;
        self.positions += o.positions;
        self.invalid_seeds += o.invalid_seeds;
        self.inferences += o.inferences;
        self.mated += o.mated;
        self.declaration += o.declaration;
        self.repetition += o.repetition;
        self.perpetual_check += o.perpetual_check;
        self.max_ply += o.max_ply;
    }
}

/// 終わった指し進めの局面を書き出す。
fn flush_game<W: Write>(
    game: Game,
    mode: GameResultMode,
    writer: &mut W,
    stats: &mut ChunkStats,
) -> Result<()> {
    let ending = game.ending.expect("flush_game is called only for finished games");
    match ending.reason {
        EndReason::Mated => stats.mated += 1,
        EndReason::Declaration => stats.declaration += 1,
        EndReason::Repetition => stats.repetition += 1,
        EndReason::PerpetualCheck => stats.perpetual_check += 1,
        EndReason::MaxPly => stats.max_ply += 1,
    }
    stats.rollouts += 1;
    for (mut psv, stm) in game.records {
        psv.game_result = match (mode, ending.winner) {
            (GameResultMode::Rollout, Some(w)) if w == stm => 1,
            (GameResultMode::Rollout, Some(_)) => -1,
            _ => 0,
        };
        writer.write_all(&psv.to_bytes())?;
        stats.positions += 1;
    }
    Ok(())
}

/// 種 `seeds` (入力ファイルの識別子 `stream` の中で、先頭の通し番号が `first_seed_index`) から
/// 指し進め、局面を `writer` に書く。
///
/// 出力は指し進めが終わった順。各指し進めの手順は自身の乱数列だけで決まり、終わる順も
/// 入力と option だけで決まるので、同じ条件の再実行は bit 一致する。
pub fn run_chunk<M: PolicyValueModel, W: Write>(
    model: &mut M,
    seeds: &[PackedSfenValue],
    stream: u64,
    first_seed_index: u64,
    cfg: &RolloutConfig,
    writer: &mut W,
    interrupted: &AtomicBool,
) -> Result<ChunkStats> {
    let mut stats = ChunkStats::default();
    let mut tasks = seeds
        .iter()
        .enumerate()
        .flat_map(|(i, s)| (0..cfg.rollouts_per_seed).map(move |r| (i, s, r)));
    let mut slots: Vec<Option<Game>> = (0..cfg.parallel).map(|_| None).collect();
    let mut tasks_done = false;
    let mut active: Vec<usize> = Vec::with_capacity(cfg.parallel);
    let mut row_of_slot = vec![usize::MAX; cfg.parallel];
    let mut values: Vec<i32> = Vec::with_capacity(cfg.parallel);
    let mut policy: Vec<f32> = Vec::with_capacity(cfg.parallel * MAX_MOVE_LABEL_NUM);

    loop {
        if interrupted.load(Ordering::SeqCst) {
            anyhow::bail!("interrupted");
        }

        // 空いた枠に新しい指し進めを入れる。種の時点で終わっている場合はすぐ書き出す。
        for slot in slots.iter_mut() {
            while slot.is_none() && !tasks_done {
                let Some((i, seed_psv, r)) = tasks.next() else {
                    tasks_done = true;
                    break;
                };
                let mut pos = Position::new();
                let ok = unpack_sfen(&seed_psv.sfen)
                    .ok()
                    .is_some_and(|sfen| pos.set_sfen(&sfen).is_ok());
                if !ok {
                    if r == 0 {
                        stats.invalid_seeds += 1;
                    }
                    continue;
                }
                let seed_move = psv_move16_to_move(seed_psv.move16);
                let avoid_first = (cfg.avoid_seed_move
                    && seed_psv.move16 != 0
                    && is_legal_psv_move(&pos, seed_move))
                .then_some(seed_move);
                let mut game = Game {
                    pos,
                    rng: rollout_rng(cfg.seed, stream, first_seed_index + i as u64, r),
                    game_ply: seed_psv.game_ply,
                    moves_from_seed: 0,
                    emit_seed: r == 0,
                    avoid_first,
                    legal: MoveList::new(),
                    ending: None,
                    records: Vec::new(),
                };
                game.update_status(cfg.max_game_ply);
                if game.ending.is_some() {
                    flush_game(game, cfg.game_result, writer, &mut stats)?;
                } else {
                    *slot = Some(game);
                }
            }
        }

        active.clear();
        active.extend((0..slots.len()).filter(|&i| slots[i].is_some()));
        if active.is_empty() {
            break;
        }

        values.clear();
        policy.clear();
        {
            let positions: Vec<&Position> =
                active.iter().map(|&i| &slots[i].as_ref().unwrap().pos).collect();
            model.infer(&positions, &mut values, &mut policy)?;
        }
        if values.len() != active.len() || policy.len() != active.len() * MAX_MOVE_LABEL_NUM {
            anyhow::bail!(
                "model returned {} values / {} policy logits for {} positions",
                values.len(),
                policy.len(),
                active.len()
            );
        }
        stats.inferences += 1;

        for (row, &slot) in active.iter().enumerate() {
            row_of_slot[slot] = row;
        }

        slots.par_iter_mut().enumerate().for_each_init(
            || (Vec::with_capacity(600), Vec::with_capacity(600)),
            |(logits, scratch), (slot_idx, slot)| {
                let Some(game) = slot.as_mut() else {
                    return;
                };
                let row = row_of_slot[slot_idx];
                let stm = game.pos.side_to_move();
                let row_logits = &policy[row * MAX_MOVE_LABEL_NUM..(row + 1) * MAX_MOVE_LABEL_NUM];
                logits.clear();
                logits.extend(game.legal.iter().map(|&mv| row_logits[make_move_label(mv, stm)]));
                let excluded = match game.avoid_first {
                    Some(avoid) if game.moves_from_seed == 0 && game.legal.len() > 1 => {
                        game.legal.iter().position(|&m| same_psv_move(m, avoid))
                    }
                    _ => None,
                };
                let mv = game.legal.as_slice()
                    [choose_move(logits, excluded, cfg, &mut game.rng, scratch)];

                if game.moves_from_seed > 0 || game.emit_seed {
                    let score = values[row].clamp(i32::from(i16::MIN) + 1, i32::from(i16::MAX));
                    game.records.push((
                        PackedSfenValue {
                            sfen: pack_position(&game.pos),
                            score: score as i16,
                            move16: move_to_psv_move16(mv),
                            game_ply: game.game_ply,
                            game_result: 0,
                            padding: game.moves_from_seed.min(u32::from(u8::MAX)) as u8,
                        },
                        stm,
                    ));
                }

                let gives_check = game.pos.gives_check(mv);
                game.pos.do_move(mv, gives_check);
                game.moves_from_seed += 1;
                game.game_ply = game.game_ply.saturating_add(1);
                game.update_status(cfg.max_game_ply);
            },
        );

        for slot in slots.iter_mut() {
            if slot.as_ref().is_some_and(|g| g.ending.is_some()) {
                let game = slot.take().unwrap();
                flush_game(game, cfg.game_result, writer, &mut stats)?;
            }
        }
    }
    Ok(stats)
}

/// chunk 番号に対応する出力ファイル名
pub fn chunk_file_name(chunk: u64) -> String {
    format!("rollout-{chunk:06}.psv")
}

/// 入力の全 chunk (`chunk_seeds` 件ずつ) を順に処理する。完成済みの chunk ファイルがあれば飛ばす。
/// `stream` は入力ファイルの識別子で、乱数列をファイルごとに分ける。
///
/// chunk は `<name>.tmp` に書いてから rename するので、中断した chunk は再実行で作り直される。
/// `readonly` は出力で上書きしてはならない資源 (入力・モデル等)。
pub fn run_all<M: PolicyValueModel>(
    model: &mut M,
    input: &Path,
    stream: u64,
    output_dir: &Path,
    chunk_seeds: u64,
    cfg: &RolloutConfig,
    readonly: &[&Path],
    interrupted: &AtomicBool,
) -> Result<ChunkStats> {
    let input_size = fs::metadata(input)
        .with_context(|| format!("cannot stat {}", input.display()))?
        .len();
    let rec = PackedSfenValue::SIZE as u64;
    if input_size % rec != 0 {
        anyhow::bail!(
            "input size {input_size} is not a multiple of {rec} (PSV record size): {}",
            input.display()
        );
    }
    let total_seeds = input_size / rec;
    let num_chunks = total_seeds.div_ceil(chunk_seeds);
    eprintln!(
        "Input: {} ({total_seeds} seeds, {num_chunks} chunks of {chunk_seeds})",
        input.display()
    );
    fs::create_dir_all(output_dir)
        .with_context(|| format!("cannot create {}", output_dir.display()))?;

    let mut reader = BufReader::new(
        File::open(input).with_context(|| format!("cannot open {}", input.display()))?,
    );
    let mut total = ChunkStats::default();
    let mut buf = [0u8; PackedSfenValue::SIZE];
    let started = Instant::now();

    for chunk in 0..num_chunks {
        let final_path = output_dir.join(chunk_file_name(chunk));
        if final_path.exists() {
            eprintln!("chunk {chunk}: {} exists, skipped", final_path.display());
            continue;
        }
        let first = chunk * chunk_seeds;
        let count = chunk_seeds.min(total_seeds - first);
        reader.seek(SeekFrom::Start(first * rec))?;
        let mut seeds = Vec::with_capacity(count as usize);
        for _ in 0..count {
            reader.read_exact(&mut buf)?;
            let psv = PackedSfenValue::from_bytes(&buf).context("malformed PSV record")?;
            seeds.push(psv);
        }

        let tmp_path = output_dir.join(format!("{}.tmp", chunk_file_name(chunk)));
        let mut writer = BufWriter::new(create_safe_output_file(&tmp_path, readonly)?);
        let chunk_started = Instant::now();
        let stats = run_chunk(model, &seeds, stream, first, cfg, &mut writer, interrupted)?;
        let file = writer.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, &final_path).with_context(|| {
            format!("cannot rename {} to {}", tmp_path.display(), final_path.display())
        })?;

        let secs = chunk_started.elapsed().as_secs_f64();
        eprintln!(
            "chunk {chunk}: {} positions from {} rollouts in {secs:.1}s ({:.0} pos/s, {} inferences, \
             avg batch {:.0}); ends: mated {} / declaration {} / repetition {} / perpetual {} / \
             max-ply {}; invalid seeds {}",
            stats.positions,
            stats.rollouts,
            stats.positions as f64 / secs.max(1e-9),
            stats.inferences,
            (stats.positions + stats.rollouts) as f64 / stats.inferences.max(1) as f64,
            stats.mated,
            stats.declaration,
            stats.repetition,
            stats.perpetual_check,
            stats.max_ply,
            stats.invalid_seeds,
        );
        total.add(&stats);
    }
    let secs = started.elapsed().as_secs_f64();
    eprintln!(
        "Done: {} positions from {} rollouts in {secs:.1}s ({:.0} pos/s)",
        total.positions,
        total.rollouts,
        total.positions as f64 / secs.max(1e-9),
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 局面の hash と手のラベルだけで決まる、GPU 不要の推論器
    struct HashModel;

    impl PolicyValueModel for HashModel {
        fn infer(
            &mut self,
            positions: &[&Position],
            values: &mut Vec<i32>,
            policy: &mut Vec<f32>,
        ) -> Result<()> {
            for pos in positions {
                let key = pos.key();
                values.push((key % 2001) as i32 - 1000);
                policy.extend((0..MAX_MOVE_LABEL_NUM).map(|l| {
                    let h = key ^ (l as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    ((h >> 40) % 1000) as f32 / 200.0
                }));
            }
            Ok(())
        }
    }

    fn config() -> RolloutConfig {
        RolloutConfig {
            rollouts_per_seed: 3,
            parallel: 4,
            temperature: 1.0,
            min_prob: 0.01,
            top_k: 0,
            max_game_ply: 60,
            game_result: GameResultMode::Rollout,
            seed: 7,
            avoid_seed_move: true,
        }
    }

    fn seed_psv(sfen: &str, game_ply: u16) -> PackedSfenValue {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        PackedSfenValue {
            sfen: pack_position(&pos),
            score: 0,
            move16: 0,
            game_ply,
            game_result: 0,
            padding: 0,
        }
    }

    fn seeds() -> Vec<PackedSfenValue> {
        vec![
            seed_psv("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1", 1),
            seed_psv("lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3", 3),
            seed_psv(
                "ln1g1g1nl/1rs1k1sb1/p1pppp1pp/6p2/1p7/2P6/PPSPPPPPP/1B1K3R1/LN1G1GSNL w - 20",
                20,
            ),
        ]
    }

    fn run_seeds(cfg: &RolloutConfig, seeds: &[PackedSfenValue]) -> (Vec<u8>, ChunkStats) {
        let mut out = Vec::new();
        let stop = AtomicBool::new(false);
        let stats = run_chunk(&mut HashModel, seeds, 0, 0, cfg, &mut out, &stop).unwrap();
        (out, stats)
    }

    fn run_bytes(cfg: &RolloutConfig) -> (Vec<u8>, ChunkStats) {
        run_seeds(cfg, &seeds())
    }

    fn records(bytes: &[u8]) -> Vec<PackedSfenValue> {
        bytes
            .as_chunks::<{ PackedSfenValue::SIZE }>()
            .0
            .iter()
            .map(|c| PackedSfenValue::from_bytes(c).unwrap())
            .collect()
    }

    #[test]
    fn same_seed_is_bit_identical_and_other_seed_differs() {
        let cfg = config();
        let (a, sa) = run_bytes(&cfg);
        let (b, sb) = run_bytes(&cfg);
        assert!(!a.is_empty());
        assert_eq!(a, b);
        assert_eq!(sa, sb);
        assert_eq!(sa.rollouts, 9);

        let (c, _) = run_bytes(&RolloutConfig { seed: 8, ..cfg });
        assert_ne!(a, c);
    }

    #[test]
    fn rollouts_do_not_depend_on_batch_layout() {
        let cfg = config();
        let sorted = |bytes: Vec<u8>| {
            let mut recs: Vec<[u8; PackedSfenValue::SIZE]> =
                records(&bytes).iter().map(|r| r.to_bytes()).collect();
            recs.sort();
            recs
        };
        let a = sorted(run_bytes(&cfg).0);
        let b = sorted(run_bytes(&RolloutConfig { parallel: 1, ..cfg }).0);
        assert_eq!(a, b);
    }

    #[test]
    fn records_follow_move_and_ply_and_stop_at_max_ply() {
        let cfg = RolloutConfig {
            game_result: GameResultMode::Zero,
            ..config()
        };
        let (bytes, stats) = run_bytes(&cfg);
        let recs = records(&bytes);
        assert_eq!(recs.len() as u64, stats.positions);
        for r in &recs {
            assert!(r.game_ply < cfg.max_game_ply);
            assert_eq!(r.game_result, 0);
            assert_ne!(r.move16, 0);
            let mut pos = Position::new();
            pos.set_sfen(&unpack_sfen(&r.sfen).unwrap()).unwrap();
            let mv = crate::packed_sfen::psv_move16_to_move(r.move16);
            assert!(crate::packed_sfen::is_legal_psv_move(&pos, mv));
            // padding は種から何手目か。種の game_ply (1 / 3 / 20) まで戻れる。
            assert!([1, 3, 20].contains(&(r.game_ply - u16::from(r.padding))));
        }
        // 種の局面は rollouts_per_seed 本に対して 1 回だけ出力する
        for seed in seeds() {
            let n = recs
                .iter()
                .filter(|r| r.sfen == seed.sfen && r.game_ply == seed.game_ply)
                .count();
            assert_eq!(n, 1);
        }
    }

    #[test]
    fn first_move_avoids_the_recorded_seed_move() {
        // 温度 0 で、種の move16 を最善手そのものにしておく
        let greedy = RolloutConfig {
            rollouts_per_seed: 1,
            temperature: 0.0,
            avoid_seed_move: false,
            ..config()
        };
        let seed_record = |bytes: &[u8], seed: &PackedSfenValue| {
            records(bytes)
                .into_iter()
                .find(|r| r.sfen == seed.sfen && r.padding == 0)
                .unwrap()
        };
        let plain = seeds();
        let (bytes, _) = run_seeds(&greedy, &plain);
        let with_move: Vec<PackedSfenValue> = plain
            .iter()
            .map(|s| PackedSfenValue {
                move16: seed_record(&bytes, s).move16,
                ..*s
            })
            .collect();

        let (kept, _) = run_seeds(&greedy, &with_move);
        let avoid = RolloutConfig {
            avoid_seed_move: true,
            ..greedy
        };
        let (avoided, _) = run_seeds(&avoid, &with_move);
        for s in &with_move {
            assert_eq!(seed_record(&kept, s).move16, s.move16);
            assert_ne!(seed_record(&avoided, s).move16, s.move16);
        }
    }

    #[test]
    fn mated_position_ends_with_win_for_mating_side() {
        // 後手玉 5一、先手の金 5二 を歩 5三 が支える。後手番で詰み。
        let sfen = "4k4/4G4/4P4/9/9/9/9/9/4K4 w - 10";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        let mut game = Game {
            pos,
            rng: rollout_rng(0, 0, 0, 0),
            game_ply: 10,
            moves_from_seed: 1,
            emit_seed: false,
            avoid_first: None,
            legal: MoveList::new(),
            ending: None,
            records: vec![
                (seed_psv("4k4/9/4P4/9/9/9/9/9/4K4 b G 9", 9), Color::Black),
                (seed_psv(sfen, 10), Color::White),
            ],
        };
        game.update_status(512);
        let ending = game.ending.unwrap();
        assert_eq!(ending.reason, EndReason::Mated);
        assert_eq!(ending.winner, Some(Color::Black));

        let mut out = Vec::new();
        let mut stats = ChunkStats::default();
        flush_game(game, GameResultMode::Rollout, &mut out, &mut stats).unwrap();
        let results: Vec<i8> = records(&out).iter().map(|r| r.game_result).collect();
        assert_eq!(results, vec![1, -1]);
        assert_eq!(stats.mated, 1);
    }

    #[test]
    fn choose_move_respects_min_prob_top_k_and_zero_temperature() {
        let mut rng = rollout_rng(1, 0, 2, 3);
        let mut scratch = Vec::new();
        // softmax 後の確率はおよそ [0.665, 0.245, 0.090, 0.0003]
        let logits = [3.0f32, 2.0, 1.0, -5.0];
        let base = config();

        let greedy = RolloutConfig {
            temperature: 0.0,
            ..base.clone()
        };
        assert_eq!(choose_move(&logits, None, &greedy, &mut rng, &mut scratch), 0);

        let top1 = RolloutConfig {
            top_k: 1,
            ..base.clone()
        };
        let floor = RolloutConfig {
            min_prob: 0.1,
            ..base.clone()
        };
        let mut seen = [0u32; 4];
        for _ in 0..2000 {
            assert_eq!(choose_move(&logits, None, &top1, &mut rng, &mut scratch), 0);
            seen[choose_move(&logits, None, &floor, &mut rng, &mut scratch)] += 1;
        }
        assert!(seen[0] > 0 && seen[1] > 0);
        assert_eq!(seen[2] + seen[3], 0);

        // 除いた手は最善手でも選ばない
        assert_eq!(choose_move(&logits, Some(0), &greedy, &mut rng, &mut scratch), 1);
        for _ in 0..200 {
            assert_ne!(choose_move(&logits, Some(0), &base, &mut rng, &mut scratch), 0);
        }
    }

    #[test]
    fn resume_skips_finished_chunks_and_reproduces_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("seeds.psv");
        let bytes: Vec<u8> = seeds().iter().flat_map(|s| s.to_bytes()).collect();
        fs::write(&input, bytes).unwrap();
        let out = dir.path().join("out");
        let cfg = config();
        let stop = AtomicBool::new(false);
        let readonly = [input.as_path()];

        run_all(&mut HashModel, &input, 0, &out, 2, &cfg, &readonly, &stop).unwrap();
        let first0 = fs::read(out.join(chunk_file_name(0))).unwrap();
        let first1 = fs::read(out.join(chunk_file_name(1))).unwrap();

        fs::remove_file(out.join(chunk_file_name(1))).unwrap();
        fs::write(out.join(chunk_file_name(0)), b"kept").unwrap();
        let stats = run_all(&mut HashModel, &input, 0, &out, 2, &cfg, &readonly, &stop).unwrap();
        assert_eq!(stats.rollouts, 3);
        assert_eq!(fs::read(out.join(chunk_file_name(0))).unwrap(), b"kept");
        assert_eq!(fs::read(out.join(chunk_file_name(1))).unwrap(), first1);
        assert!(!first0.is_empty());
    }
}
