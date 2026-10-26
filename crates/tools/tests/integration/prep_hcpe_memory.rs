use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path, process::Command};

fn generate_input(path: &Path, count: u64) {
    let mut writer = std::io::BufWriter::new(fs::File::create(path).unwrap());
    for index in 0..count {
        let mut record = [0; 38];
        record[..32].copy_from_slice(&Sha256::digest(index.to_le_bytes()));
        record[32..].copy_from_slice(&index.to_le_bytes()[..6]);
        writer.write_all(&record).unwrap();
    }
    writer.flush().unwrap();
}

#[test]
fn shuffle_budget_selects_both_methods_with_reproducible_complete_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.hcpe");
    let count = 20_000;
    generate_input(&input, count);
    let mut memory = None;
    let mut disk = None;
    for (name, budget) in [
        ("memory-a", None),
        ("memory-b", None),
        ("disk-a", Some("1")),
        ("disk-b", Some("1")),
    ] {
        let output = dir.path().join(name);
        let mut command = Command::new(env!("CARGO_BIN_EXE_prep_hcpe"));
        command.arg("--in").arg(&input).arg("--out-dir").arg(&output).args([
            "--seed",
            "42",
            "--expected-records",
            "320000",
            "--false-positive-rate",
            "0.000000001",
        ]);
        if let Some(budget) = budget {
            command.args(["--shuffle-memory-mib", budget]);
        }
        let result = command.output().unwrap();
        let report = String::from_utf8(result.stderr).unwrap();
        assert!(result.status.success(), "{report}");
        assert!(report.contains("Bloom filter:"));
        assert!(report.contains("プロセス全体の上限ではありません"));
        assert!(String::from_utf8_lossy(&result.stdout).contains("written=20000"));
        let bytes = fs::read(output.join("chunk_00000.hcpe")).unwrap();
        assert_eq!(bytes.len(), count as usize * 38);
        if budget.is_none() {
            assert!(report.contains("初期保持バッファ:"));
            assert!(report.contains("shuffle: メモリ内 shuffle"));
            assert!(!report.contains("shuffle: ディスクへ切替"));
            if let Some(previous) = &memory {
                assert_eq!(&bytes, previous);
            }
            memory = Some(bytes);
        } else {
            assert!(report.contains("shuffle: ディスクへ切替"));
            assert!(report.contains("shuffle: 外部ソート"));
            if let Some(previous) = &disk {
                assert_eq!(&bytes, previous);
            }
            disk = Some(bytes);
        }
    }
    let sorted = |bytes: Vec<u8>| {
        let mut records: Vec<_> = bytes.as_chunks::<38>().0.to_vec();
        records.sort_unstable();
        records
    };
    assert_eq!(sorted(memory.unwrap()), sorted(disk.unwrap()));
}

#[test]
fn invalid_shuffle_budgets_fail_before_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.hcpe");
    generate_input(&input, 2);
    for args in [
        vec!["--shuffle-memory-mib", "0"],
        vec!["--shuffle-memory-mib", "18446744073709551615"],
        vec!["--target", "1", "--shuffle-memory-mib", "1"],
    ] {
        let output = dir.path().join("out");
        let result = Command::new(env!("CARGO_BIN_EXE_prep_hcpe"))
            .arg("--in")
            .arg(&input)
            .arg("--out-dir")
            .arg(&output)
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("--shuffle-memory-mib"));
        assert!(!output.exists());
    }
}
