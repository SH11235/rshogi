use clap::{Arg, Command};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use tools::common::io::open_reader;
use tools::common::sfen::normalize_4t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitKind {
    Train,
    Valid,
    Test,
}

fn fingerprint_sfen(s: &str) -> u128 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let d = hasher.finalize();
    u128::from_le_bytes(d[0..16].try_into().unwrap())
}

#[derive(Clone)]
struct FirstSeen {
    set: SplitKind,
    path: String,
    line: usize,
}

fn ingest(
    set: SplitKind,
    path: &str,
    report_same_set: bool,
    first: &mut HashMap<u128, FirstSeen>,
    leaks: &mut Vec<(String, SplitKind, String, usize, SplitKind, String, usize)>,
) -> std::io::Result<()> {
    let reader = open_reader(path)?;
    for (line_idx, line) in reader.lines().enumerate() {
        let line_no = line_idx + 1;
        let l = match line {
            Ok(s) => s,
            Err(_) => continue,
        };
        if l.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&l) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let sfen = match v.get("sfen").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let key = match normalize_4t(sfen) {
            Some(k) => k,
            None => continue,
        };
        let fp = fingerprint_sfen(&key);
        if let Some(fs) = first.get(&fp) {
            // Report cross-set always; report same-set when `report_same_set` is true
            if fs.set != set || report_same_set {
                leaks.push((key, fs.set, fs.path.clone(), fs.line, set, path.to_string(), line_no));
            }
        } else {
            // Only record first occurrence
            first.insert(
                fp,
                FirstSeen {
                    set,
                    path: path.to_string(),
                    line: line_no,
                },
            );
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Command::new("check_cross_dedup")
        .about("Check cross/intra duplicates between train/valid/test (SFEN normalized to first 4 tokens)")
        .arg(Arg::new("train").long("train").value_name("FILE").required(true))
        .arg(Arg::new("valid").long("valid").value_name("FILE").required(true))
        .arg(Arg::new("test").long("test").value_name("FILE").required(true))
        .arg(Arg::new("report").long("report").value_name("FILE").required(true))
        .arg(
            Arg::new("include-intra")
                .long("include-intra")
                .action(clap::ArgAction::SetTrue)
                .help("Also report duplicates within the same split (in addition to cross-set)"),
        )
        .get_matches();
    let train = matches.get_one::<String>("train").unwrap();
    let valid = matches.get_one::<String>("valid").unwrap();
    let test = matches.get_one::<String>("test").unwrap();
    let report = matches.get_one::<String>("report").unwrap();
    let include_intra = *matches.get_one::<bool>("include-intra").unwrap_or(&false);

    let mut leaks: Vec<(String, SplitKind, String, usize, SplitKind, String, usize)> = Vec::new();

    if include_intra {
        // Intra detection per split (report_same_set = true)
        let mut map_train: HashMap<u128, FirstSeen> = HashMap::new();
        ingest(SplitKind::Train, train, true, &mut map_train, &mut leaks)?;
        let mut map_valid: HashMap<u128, FirstSeen> = HashMap::new();
        ingest(SplitKind::Valid, valid, true, &mut map_valid, &mut leaks)?;
        let mut map_test: HashMap<u128, FirstSeen> = HashMap::new();
        ingest(SplitKind::Test, test, true, &mut map_test, &mut leaks)?;
        // Cross detection (report_same_set = false)
        let mut first: HashMap<u128, FirstSeen> = HashMap::new();
        ingest(SplitKind::Train, train, false, &mut first, &mut leaks)?;
        ingest(SplitKind::Valid, valid, false, &mut first, &mut leaks)?;
        ingest(SplitKind::Test, test, false, &mut first, &mut leaks)?;
    } else {
        // Cross-only
        let mut first: HashMap<u128, FirstSeen> = HashMap::new();
        ingest(SplitKind::Train, train, false, &mut first, &mut leaks)?;
        ingest(SplitKind::Valid, valid, false, &mut first, &mut leaks)?;
        ingest(SplitKind::Test, test, false, &mut first, &mut leaks)?;
    }

    // Write CSV report
    {
        let mut out = csv::Writer::from_path(PathBuf::from(report))?;
        out.write_record([
            "sfen_key",
            "first_set",
            "dup_set",
            "first_path",
            "dup_path",
            "first_line",
            "dup_line",
        ])?;
        for (key, fset, fpath, fline, dset, dpath, dline) in &leaks {
            let fs = match fset {
                SplitKind::Train => "train",
                SplitKind::Valid => "valid",
                SplitKind::Test => "test",
            };
            let ds = match dset {
                SplitKind::Train => "train",
                SplitKind::Valid => "valid",
                SplitKind::Test => "test",
            };
            out.write_record([
                key,
                fs,
                ds,
                fpath,
                dpath,
                &fline.to_string(),
                &dline.to_string(),
            ])?;
        }
        out.flush()?;
    }

    // Pairwise summary for CI logs
    let mut tv = 0usize; // train<->valid
    let mut tt = 0usize; // train<->test
    let mut vt = 0usize; // valid<->test
    let mut tr_in = 0usize; // train intra
    let mut va_in = 0usize; // valid intra
    let mut te_in = 0usize; // test intra
    for (_, fset, _, _, dset, _, _) in &leaks {
        match (fset, dset) {
            (SplitKind::Train, SplitKind::Valid) | (SplitKind::Valid, SplitKind::Train) => tv += 1,
            (SplitKind::Train, SplitKind::Test) | (SplitKind::Test, SplitKind::Train) => tt += 1,
            (SplitKind::Valid, SplitKind::Test) | (SplitKind::Test, SplitKind::Valid) => vt += 1,
            (SplitKind::Train, SplitKind::Train) => tr_in += 1,
            (SplitKind::Valid, SplitKind::Valid) => va_in += 1,
            (SplitKind::Test, SplitKind::Test) => te_in += 1,
        }
    }
    println!(
        "SUMMARY: cross tv={} tt={} vt={} | intra tr={} va={} te={} | total={}",
        tv,
        tt,
        vt,
        tr_in,
        va_in,
        te_in,
        leaks.len()
    );

    if !leaks.is_empty() {
        if include_intra {
            let (cross, intra) =
                leaks.iter().fold((0usize, 0usize), |(c, i), (_, fset, _, _, dset, _, _)| {
                    if fset == dset {
                        (c, i + 1)
                    } else {
                        (c + 1, i)
                    }
                });
            eprintln!(
                "Found {} duplicates (cross={}, intra={}); see {}",
                leaks.len(),
                cross,
                intra,
                report
            );
        } else {
            eprintln!("Found {} cross-set duplicates; see {}", leaks.len(), report);
        }
        std::process::exit(2);
    }
    println!(
        "{}",
        if include_intra {
            "No duplicates detected"
        } else {
            "No cross-set duplicates detected"
        }
    );
    Ok(())
}
