use clap::{Arg, Command};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use tools::common::io::open_reader;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitKind {
    Train,
    Valid,
    Test,
}

fn normalize_sfen_tokens(sfen: &str) -> Option<String> {
    let mut it = sfen.split_whitespace();
    let b = it.next()?;
    let s = it.next()?;
    let h = it.next()?;
    let m = it.next()?;
    Some(format!("{} {} {} {}", b, s, h, m))
}

fn fingerprint_sfen(s: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let d = hasher.finalize();
    u64::from_le_bytes(d[0..8].try_into().unwrap())
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
    first: &mut HashMap<u64, FirstSeen>,
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
        let key = match normalize_sfen_tokens(sfen) {
            Some(k) => k,
            None => continue,
        };
        let fp = fingerprint_sfen(&key);
        if let Some(fs) = first.get(&fp) {
            // Only cross-set duplicates are reported by default
            if fs.set != set {
                leaks.push((key, fs.set, fs.path.clone(), fs.line, set, path.to_string(), line_no));
            }
        } else {
            // Only record first occurrence; cross-dedup only
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
        .about("Check cross-dedup between train/valid/test JSONL datasets (by SFEN key)")
        .arg(Arg::new("train").long("train").value_name("FILE").required(true))
        .arg(Arg::new("valid").long("valid").value_name("FILE").required(true))
        .arg(Arg::new("test").long("test").value_name("FILE").required(true))
        .arg(Arg::new("report").long("report").value_name("FILE").required(true))
        .get_matches();
    let train = matches.get_one::<String>("train").unwrap();
    let valid = matches.get_one::<String>("valid").unwrap();
    let test = matches.get_one::<String>("test").unwrap();
    let report = matches.get_one::<String>("report").unwrap();

    let mut first: HashMap<u64, FirstSeen> = HashMap::new();
    let mut leaks: Vec<(String, SplitKind, String, usize, SplitKind, String, usize)> = Vec::new();

    ingest(SplitKind::Train, train, &mut first, &mut leaks)?;
    ingest(SplitKind::Valid, valid, &mut first, &mut leaks)?;
    ingest(SplitKind::Test, test, &mut first, &mut leaks)?;

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

    if !leaks.is_empty() {
        eprintln!("Found {} cross-set duplicates; see {}", leaks.len(), report);
        std::process::exit(2);
    }
    println!("No cross-set duplicates detected");
    Ok(())
}
