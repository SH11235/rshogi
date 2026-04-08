//! NNUE accumulator 検証ツール (refresh vs differential update 一致テスト)
//!
//! quantised.bin を読み込み、startpos から手を進めながら:
//! 1. move 前の局面で refresh → accumulator を保存
//! 2. do_move + update (differential) → evaluate
//! 3. do_move 後に refresh → evaluate
//! 4. (2) と (3) の評価値が完全一致することを検証
//!
//! PSQT / Threat / PSQT+Threat / 素の LayerStacks 全てに対応。
//!
//! ```bash
//! cargo run --release --bin verify_nnue_accumulator -- \
//!   --nnue-file path/to/quantised.bin \
//!   --ls-progress-coeff path/to/nodchip_progress_e1_f1_cuda.bin
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::nnue::{
    AccumulatorLayerStacks, LayerStackBucketMode, LayerStacksNetwork, NNUENetwork,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, set_layer_stack_bucket_mode,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;

#[derive(Parser, Debug)]
#[command(
    name = "verify_nnue_accumulator",
    about = "NNUE accumulator 検証 (refresh vs differential update 一致テスト)"
)]
struct Cli {
    #[arg(long)]
    nnue_file: PathBuf,

    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,

    /// テスト手数 (default: 50)
    #[arg(long, default_value = "50")]
    moves: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Bucket mode 設定
    if let Some(ref coeff_path) = cli.ls_progress_coeff {
        let data = std::fs::read(coeff_path)
            .with_context(|| format!("Failed to read progress coeff: {}", coeff_path.display()))?;
        if data.len() == SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * 4 {
            let weights: Vec<f32> = data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            set_layer_stack_progress_kpabs_weights(weights.into_boxed_slice())
                .expect("progress kpabs weights");
            set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
            println!("Bucket mode: progress8kpabs");
        }
    }

    // NNUE モデル読み込み
    let network = NNUENetwork::load(&cli.nnue_file)
        .with_context(|| format!("Failed to load NNUE: {}", cli.nnue_file.display()))?;

    let net = match &network {
        NNUENetwork::LayerStacks(n) => n,
        _ => anyhow::bail!("Expected LayerStacks network"),
    };

    println!("Model loaded successfully (L1={}).", net.l1_size());

    /// L1 dispatch: LayerStacksNetwork の各バリアントに対して同一ロジックを実行
    macro_rules! with_net {
        ($net:expr, |$inner:ident| $body:expr) => {
            match $net {
                #[cfg(feature = "layerstacks-1536")]
                LayerStacksNetwork::L1536($inner) => $body,
                #[cfg(feature = "layerstacks-768")]
                LayerStacksNetwork::L768($inner) => $body,
                #[allow(unreachable_patterns)]
                _ => anyhow::bail!("有効な LayerStacks バリアントがありません"),
            }
        };
    }

    // テスト SFEN
    let sfens = ["lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"];

    let mut total_tests = 0;
    let mut fail = 0;

    with_net!(net, |concrete_net| {
        for sfen in &sfens {
            let mut pos = Position::new();
            pos.set_sfen(sfen).with_context(|| format!("Bad SFEN: {sfen}"))?;

            for step in 0..cli.moves {
                let mut moves = MoveList::new();
                generate_legal_all(&pos, &mut moves);
                if moves.is_empty() {
                    println!("  No legal moves at step {step}, restarting.");
                    pos.set_sfen(sfen)?;
                    continue;
                }

                // 決定的に最初の合法手を選択
                let m = moves[0];
                let gc = pos.gives_check(m);

                // Before move: refresh accumulator
                let mut acc_before = AccumulatorLayerStacks::new();
                concrete_net.refresh_accumulator(&pos, &mut acc_before);

                // Do move
                let dirty = pos.do_move(m, gc);

                // After move: refresh (golden truth)
                let mut acc_refresh = AccumulatorLayerStacks::new();
                concrete_net.refresh_accumulator(&pos, &mut acc_refresh);
                let eval_refresh = concrete_net.evaluate(&pos, &acc_refresh);

                // After move: update (differential from acc_before)
                let mut acc_update = AccumulatorLayerStacks::new();
                concrete_net.update_accumulator(&pos, &dirty, &mut acc_update, &acc_before);
                let eval_update = concrete_net.evaluate(&pos, &acc_update);

                total_tests += 1;

                if eval_refresh != eval_update {
                    fail += 1;
                    eprintln!(
                        "MISMATCH step={step} move={m:?}: refresh={} update={}",
                        eval_refresh.raw(),
                        eval_update.raw()
                    );
                    for p in 0..2 {
                        let r = acc_refresh.get(p);
                        let u = acc_update.get(p);
                        let diffs: usize = r.iter().zip(u.iter()).filter(|(a, b)| a != b).count();
                        if diffs > 0 {
                            eprintln!("  piece_acc[{p}]: {diffs}/{} differ", r.len());
                        }
                        #[cfg(feature = "nnue-threat")]
                        {
                            let rt = acc_refresh.get_threat(p);
                            let ut = acc_update.get_threat(p);
                            let tdiffs: usize =
                                rt.iter().zip(ut.iter()).filter(|(a, b)| a != b).count();
                            if tdiffs > 0 {
                                eprintln!("  threat_acc[{p}]: {tdiffs}/{} differ", rt.len());
                            }
                        }
                    }
                    if fail >= 10 {
                        eprintln!("Too many failures, stopping.");
                        break;
                    }
                }
            }
        }
    });

    println!("\n=== Golden Forward Test Results ===");
    println!("Total: {total_tests}, Pass: {}, Fail: {fail}", total_tests - fail);

    if fail > 0 {
        anyhow::bail!("{fail}/{total_tests} tests FAILED");
    }
    println!("ALL PASSED");
    Ok(())
}
