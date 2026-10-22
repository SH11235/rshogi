//! df-pn 詰み探索の局面集ベンチ (wasm 向けの最小版)
//!
//! `crates/tools` の `mate_bench` と同じ局面の選び方で解き、結果の内訳と合計ノード数・時間を出す
//! (分位点や正解との照合は無い)。rshogi-core だけに依存する 1 スレッドの処理で、wasm
//! (ブラウザに組み込むときと同じコード生成) での速さを測るために WASI 向けにビルドして Node.js で動かす。
//! 手順は `crates/tools/docs/mate_bench.md` の「wasm で測る」。
//!
//! 引数: `--input <file> [--stride N] [--offset N] [--limit N] [--flip] [--nodes N] [--hash-mb N] [--out <tsv>]`
//! 終了コード: 引数の誤りは 2、実行中の失敗は 1。

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::str::FromStr;
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
        let value = it
            .next()
            .filter(|v| !v.starts_with("--"))
            .ok_or_else(|| format!("{flag} の値が無い"))?;
        fn number<T: FromStr>(flag: &str, value: &str) -> Result<T, String> {
            value
                .parse()
                .map_err(|_| format!("{flag} {value}: 数として読めない (範囲外を含む)"))
        }
        match flag.as_str() {
            "--input" => args.input = value,
            "--out" => args.out = Some(value),
            "--stride" => args.stride = number(&flag, &value)?,
            "--offset" => args.offset = number(&flag, &value)?,
            "--limit" => args.limit = number(&flag, &value)?,
            "--nodes" => args.nodes = number(&flag, &value)?,
            "--hash-mb" => args.hash_mb = number(&flag, &value)?,
            _ => return Err(format!("知らない引数: {flag}")),
        }
    }
    if args.input.is_empty() {
        return Err("--input が要る".to_string());
    }
    if args.stride == 0 || args.offset >= args.stride {
        return Err("--offset は --stride 未満にする".to_string());
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

fn run(args: &Args) -> Result<(), String> {
    let file =
        std::fs::File::open(&args.input).map_err(|e| format!("{} を開けない: {e}", args.input))?;
    // WASI ではパスの正規化 (別名・リンクの判定) ができないので、既存のファイルには書かないことで
    // 入力を上書きしないようにする
    let mut out = match &args.out {
        Some(path) => {
            let file =
                std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(
                    |e| format!("{path} を新しく作れない (既にあるファイルには書かない): {e}"),
                )?;
            let mut w = BufWriter::new(file);
            writeln!(w, "sfen\tresult\tpv_len\tnodes\tmicros").map_err(|e| e.to_string())?;
            Some(w)
        }
        None => None,
    };

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
        let line = line.map_err(|e| format!("{} を読めない: {e}", args.input))?;
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
        pos.set_sfen(&sfen).map_err(|e| format!("SFEN を読めない: {sfen}: {e:?}"))?;
        let flipped = (args.flip && !pos.in_check()).then(|| flip_side_to_move(&sfen));
        for s in std::iter::once(sfen).chain(flipped) {
            let mut pos = Position::new();
            pos.set_sfen(&s).map_err(|e| format!("SFEN を読めない: {s}: {e:?}"))?;
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
                    .map_err(|e| e.to_string())?;
            }
            solved += 1;
            if args.limit != 0 && solved >= args.limit {
                break 'lines;
            }
        }
    }
    if let Some(mut w) = out {
        w.flush().map_err(|e| e.to_string())?;
    }
    let wall_ms = start.elapsed().as_millis().max(1) as u64;
    println!(
        "局面 {solved}: mate {mate} / nomate {nomate} / unknown {unknown} / unsupported {unsupported}"
    );
    println!("nodes {total_nodes}、壁時計 {wall_ms} ms、{} knps", total_nodes / wall_ms);
    Ok(())
}

fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    // 失敗しても、それまでに解いた局面の TSV は `run` の中の BufWriter の破棄で書き出される
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
