use std::process::Command;
use std::sync::Arc;

use rshogi_core::nnue::net_delta::test_utils::build_synthetic_layer_stacks;
use rshogi_core::nnue::{HALFKA_HM_DIMENSIONS, NNUEEvaluator, NNUENetwork};
use rshogi_core::position::{Position, SFEN_HIRATE};
use tempfile::tempdir;

#[test]
fn eval_sfens_spec_matches_engine_and_checks_cli_conflicts() {
    let temp = tempdir().unwrap();
    let net = temp.path().join("spec.bin");
    let sfens = temp.path().join("positions.sfens");

    let mut model =
        build_synthetic_layer_stacks("HalfKaHmMerged", HALFKA_HM_DIMENSIONS, 1536, 16, 64, 16);
    for (bucket, offsets) in model.buckets.iter().enumerate() {
        let bias = (bucket as i32 * 4096).to_le_bytes();
        model.bytes[offsets.output_bias..offsets.output_bias + bias.len()].copy_from_slice(&bias);
    }
    let arch_len = u32::from_le_bytes(model.bytes[8..12].try_into().unwrap()) as usize;
    let arch_end = 12 + arch_len;
    let arch = std::str::from_utf8(&model.bytes[12..arch_end]).unwrap();
    let arch = format!("{arch},ls_bucket=ownking:2/3/7+oppking:2/3/7");
    model
        .bytes
        .splice(8..arch_end, (arch.len() as u32).to_le_bytes().into_iter().chain(arch.bytes()));
    std::fs::write(&net, &model.bytes).unwrap();
    drop(model);

    let positions = [
        (SFEN_HIRATE, 0),
        ("4k4/9/9/9/9/9/4K4/9/9 b - 1", 8),
        ("4k4/9/9/9/9/9/4K4/9/9 w - 1", 2),
        ("4K4/9/9/9/9/9/9/9/4k4 b - 1", 15),
    ];
    let input = positions.map(|(sfen, _)| sfen).join("\n");
    std::fs::write(&sfens, input).unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_eval_sfens"))
            .arg("--nnue")
            .arg(&net)
            .arg("--sfens")
            .arg(&sfens)
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut rows = stdout.lines();
    assert_eq!(rows.next(), Some("sfen\tbucket\traw\tscore\tscore_cp"));
    let network = Arc::new(NNUENetwork::load(&net).unwrap());
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    let mut evaluator = NNUEEvaluator::new_with_position(network, &pos);
    for (sfen, bucket) in positions {
        pos.set_sfen(sfen).unwrap();
        evaluator.reset(&pos);
        let score = evaluator.evaluate(&pos);
        let columns: Vec<_> = rows.next().unwrap().split('\t').collect();
        assert_eq!(columns.len(), 5);
        assert_eq!(columns[0], sfen);
        assert_eq!(columns[1].parse::<usize>().unwrap(), bucket, "{sfen}");
        assert_eq!(columns[3].parse::<i32>().unwrap(), score.raw(), "{sfen}");
        assert_eq!(columns[4].parse::<i32>().unwrap(), score.to_cp(), "{sfen}");
    }
    assert_eq!(rows.next(), None);

    let output = run(&["--bucket-mode", "progresskpabs"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, stdout.as_bytes());

    for (flag, value) in [
        ("--bucket-mode", "progresskpabsq16"),
        ("--progress-buckets", "16"),
        ("--progress-coeff", "unused.bin"),
    ] {
        let output = run(&[flag, value]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{flag}: {stderr}");
        assert!(
            stderr.contains(&format!("NNUE ls_bucket routing conflicts with {flag}")),
            "{stderr}"
        );
        assert!(output.stdout.is_empty());
    }
}
