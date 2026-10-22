//! df-pn 詰み探索の局面集ベンチ (wasm 向けの最小版)
//!
//! `crates/tools` の `mate_bench` と同じ集計を、rshogi-core だけに依存する 1 スレッドの処理で行う。
//! wasm (ブラウザに組み込むときと同じコード生成) での速さを測るために、WASI 向けにビルドして
//! Node.js で動かす。手順は `crates/tools/docs/mate_bench.md` の「wasm で測る」。
//!
//! 引数: `--input <file> [--stride N] [--offset N] [--limit N] [--flip] [--nodes N] [--hash-mb N] [--out <tsv>]`

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use rshogi_core::mate::dfpn::{DfpnSolver, MateLimits, MateResult};
use rshogi_core::position::Position;

struct Args {
    input: String,
    stride: usize,
    offset: usize,
    limit: usize,
    flip: bool,
    nodes: u64,
    hash_mb: usize,
    out: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: String::new(),
        stride: 1,
        offset: 0,
        limit: 0,
        flip: false,
        nodes: 0,
        hash_mb: 64,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--flip" {
            args.flip = true;
            continue;
        }
        let value = it.next().ok_or_else(|| format!("{flag} の値が無い"))?;
        let number = || value.parse::<u64>().map_err(|e| format!("{flag} {value}: {e}"));
        match flag.as_str() {
            "--input" => args.input = value.clone(),
            "--out" => args.out = Some(value.clone()),
            "--stride" => args.stride = number()? as usize,
            "--offset" => args.offset = number()? as usize,
            "--limit" => args.limit = number()? as usize,
            "--nodes" => args.nodes = number()?,
            "--hash-mb" => args.hash_mb = number()? as usize,
            _ => return Err(format!("知らない引数: {flag}")),
        }
    }
    if args.input.is_empty() {
        return Err("--input が要る".to_string());
    }
    if args.stride == 0 || args.offset >= args.stride {
        return Err("--offset は --stride 未満にする".to_string());
    }
    if args.out.as_deref() == Some(args.input.as_str()) {
        return Err("--out に入力と同じファイルは指定できない".to_string());
    }
    Ok(args)
}

fn flip_side_to_move(sfen: &str) -> String {
    let mut fields: Vec<&str> = sfen.split_whitespace().collect();
    if fields.len() > 1 {
        fields[1] = if fields[1] == "b" { "w" } else { "b" };
    }
    fields.join(" ")
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let file = std::fs::File::open(&args.input).unwrap_or_else(|e| {
        eprintln!("{} を開けない: {e}", args.input);
        std::process::exit(2);
    });
    let mut out = args.out.as_ref().map(|path| {
        let file = std::fs::File::create(path).unwrap_or_else(|e| {
            eprintln!("{path} を作れない: {e}");
            std::process::exit(2);
        });
        let mut w = BufWriter::new(file);
        writeln!(w, "sfen\tresult\tpv_len\tnodes\tmicros").expect("TSV に書けない");
        w
    });

    let mut solver = DfpnSolver::new(args.hash_mb);
    let limits = MateLimits {
        nodes: args.nodes,
        time_ms: 0,
    };
    let stop = AtomicBool::new(false);
    let (mut mate, mut nomate, mut unknown, mut unsupported) = (0usize, 0usize, 0usize, 0usize);
    let mut total_nodes = 0u64;
    let mut solved = 0usize;
    let mut line_index = 0usize;
    let start = Instant::now();

    'lines: for line in BufReader::new(file).lines() {
        let line = line.expect("入力を読めない");
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let selected = line_index % args.stride == args.offset;
        line_index += 1;
        if !selected {
            continue;
        }
        let sfen = line.strip_prefix("sfen ").unwrap_or(line).to_string();
        let mut pos = Position::new();
        if let Err(e) = pos.set_sfen(&sfen) {
            eprintln!("SFEN を読めない: {sfen}: {e:?}");
            std::process::exit(2);
        }
        let flipped = (args.flip && !pos.in_check()).then(|| flip_side_to_move(&sfen));
        for s in std::iter::once(sfen).chain(flipped) {
            let mut pos = Position::new();
            pos.set_sfen(&s).expect("読み込み時に検査済み");
            let t = Instant::now();
            let result = solver.solve(&mut pos, &limits, &stop, |_| {});
            let micros = t.elapsed().as_micros();
            let (tag, pv_len) = match &result {
                MateResult::Mate(pv) => {
                    mate += 1;
                    ("mate", pv.len())
                }
                MateResult::NoMate => {
                    nomate += 1;
                    ("nomate", 0)
                }
                MateResult::Unknown => {
                    unknown += 1;
                    ("unknown", 0)
                }
                MateResult::Unsupported => {
                    unsupported += 1;
                    ("unsupported", 0)
                }
            };
            total_nodes += solver.nodes();
            if let Some(w) = out.as_mut() {
                writeln!(w, "{s}\t{tag}\t{pv_len}\t{}\t{micros}", solver.nodes())
                    .expect("TSV に書けない");
            }
            solved += 1;
            if args.limit != 0 && solved >= args.limit {
                break 'lines;
            }
        }
    }
    if let Some(mut w) = out {
        w.flush().expect("TSV に書けない");
    }
    let wall_ms = start.elapsed().as_millis().max(1) as u64;
    println!(
        "局面 {solved}: mate {mate} / nomate {nomate} / unknown {unknown} / unsupported {unsupported}"
    );
    println!("nodes {total_nodes}、壁時計 {wall_ms} ms、{} knps", total_nodes / wall_ms);
}
