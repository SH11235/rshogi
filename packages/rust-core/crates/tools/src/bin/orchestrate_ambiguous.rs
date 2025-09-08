use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::Parser;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tools::common::manifest::{resolve_manifest, AutoloadMode};
use tools::common::sfen::normalize_4t;

#[derive(Parser, Debug)]
#[command(
    name = "orchestrate_ambiguous",
    about = "Extract ambiguous positions from pass1, re-annotate with stronger settings, then merge.",
    disable_help_subcommand = true
)]
struct Cli {
    /// First-pass JSONL outputs (one or more)
    #[arg(long = "pass1", value_name = "FILE", required = true)]
    pass1: Vec<PathBuf>,
    /// Final merged output path (.jsonl[.gz|.zst])
    #[arg(long = "final", value_name = "FILE", required = true)]
    final_out: PathBuf,
    /// Working directory for intermediates (default: alongside final as .<stem>.ambdig)
    #[arg(long = "out-dir", value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Orchestration manifest output path (default: <out-dir>/orchestrate_ambiguous.manifest.json)
    #[arg(long = "orchestrator-manifest-out", value_name = "FILE")]
    orchestrator_manifest_out: Option<PathBuf>,
    /// Final aggregated manifest path for merge (default: <final>.manifest.json)
    #[arg(long = "final-manifest-out", value_name = "FILE")]
    final_manifest_out: Option<PathBuf>,
    /// Backward-compat alias of --orchestrator-manifest-out
    #[arg(long = "manifest-out", hide = true)]
    legacy_manifest_out: Option<PathBuf>,
    /// Keep intermediate files (default: true)
    #[arg(long = "keep-intermediate", default_value_t = true)]
    keep_intermediate: bool,

    // Extract options (passed to extract_flagged_positions)
    /// Include when best2_gap_cp <= threshold (alias: --max-gap-cp)
    #[arg(long = "gap-threshold", alias = "max-gap-cp", default_value_t = 35)]
    gap_threshold: i64,
    /// Include non-exact records
    #[arg(long = "include-non-exact")]
    include_non_exact: bool,
    /// Include when aspiration_retries >= N
    #[arg(long = "include-aspiration-failures", value_name = "N")]
    include_aspiration_failures: Option<i64>,
    /// Include when any line has mate_distance or record has mate_boundary
    #[arg(long = "include-mate-boundary")]
    include_mate_boundary: bool,

    // Re-annotate options (passed to generate_nnue_training_data)
    #[arg(long = "engine", default_value = "enhanced")]
    engine: String,
    #[arg(long = "nnue-weights")]
    nnue_weights: Option<PathBuf>,
    #[arg(long = "teacher-profile", default_value = "balanced")]
    teacher_profile: String,
    #[arg(long = "multipv", default_value_t = 3)]
    multipv: u8,
    #[arg(long = "min-depth")]
    min_depth: Option<u8>,
    #[arg(long = "nodes")]
    nodes: Option<u64>,
    #[arg(long = "time-limit-ms")]
    time_limit_ms: Option<u64>,
    #[arg(long = "jobs")]
    jobs: Option<usize>,
    #[arg(long = "hash-mb", default_value_t = 64)]
    hash_mb: usize,
    #[arg(long = "reuse-tt")]
    reuse_tt: bool,
    #[arg(long = "split", default_value_t = 1_000_000)]
    split_every: usize,
    #[arg(long = "compress", value_name = "gz|zst")]
    compress: Option<String>,
    #[arg(long = "structured-log")]
    structured_log: Option<PathBuf>,

    // Ambiguity/entropy pass-through
    #[arg(long = "amb-gap2-threshold")]
    amb_gap2_threshold: Option<i32>,
    #[arg(long = "amb-allow-inexact")]
    amb_allow_inexact: bool,
    #[arg(long = "entropy-mate-mode", value_name = "exclude|saturate")]
    entropy_mate_mode: Option<String>,
    #[arg(long = "entropy-scale")]
    entropy_scale: Option<f64>,

    // Merge options
    /// Merge mode (default: depth-first). Always passed explicitly to ensure reproducibility.
    #[arg(long = "merge-mode", default_value = "depth-first")]
    merge_mode: String,

    // Analyze (optional)
    #[arg(long = "analyze-summary")]
    analyze_summary: bool,
    #[arg(long = "analyze-out")]
    analyze_out: Option<PathBuf>,

    // Misc
    #[arg(long = "dry-run")]
    dry_run: bool,
    #[arg(long = "verbose")]
    verbose: bool,
}

fn stem_for_artifacts(final_out: &Path) -> String {
    let name = final_out.file_name().and_then(|s| s.to_str()).unwrap_or("final");
    let name = name.strip_suffix(".zst").unwrap_or(name);
    let name = name.strip_suffix(".gz").unwrap_or(name);
    name.strip_suffix(".jsonl").unwrap_or(name).to_string()
}

fn default_out_dir(final_out: &Path) -> PathBuf {
    let dir = final_out.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".{}.ambdig", stem_for_artifacts(final_out)))
}

fn find_tool(name: &str) -> PathBuf {
    // 1) Test/CI: CARGO_BIN_EXE_<name>
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = std::env::var(&key) {
        return PathBuf::from(p);
    }
    // 2) Same dir as current exe: prefer exact, then prefix match
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            // 2a) exact
            #[cfg(windows)]
            let exact = dir.join(format!("{}.exe", name));
            #[cfg(not(windows))]
            let exact = dir.join(name);
            if exact.exists() {
                return exact;
            }
            // 2b) prefix scan
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if !p.is_file() {
                        continue;
                    }
                    let fname = p.file_name().and_then(OsStr::to_str).unwrap_or("");
                    #[cfg(windows)]
                    let wanted = fname.starts_with(name) && fname.ends_with(".exe");
                    #[cfg(not(windows))]
                    let wanted = fname.starts_with(name);
                    if wanted {
                        return p;
                    }
                }
            }
        }
    }
    // 3) PATH fallback
    PathBuf::from(name)
}

fn sha256_and_size(path: &Path) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn write_atomic(path: &Path, s: &str) -> Result<()> {
    let pid = std::process::id();
    let tmp = {
        let ext = path.extension().and_then(OsStr::to_str).unwrap_or("tmp");
        path.with_extension(format!("{}.tmp.{}", ext, pid))
    };
    fs::write(&tmp, s)?;
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_out_dir_strips_multi_extensions() {
        let p = Path::new("runs/final.jsonl.gz");
        let got = default_out_dir(p);
        assert_eq!(got, Path::new("runs/.final.ambdig"));
    }

    #[test]
    fn glob_pass2_outputs_sorts_naturally() {
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, b"\n").unwrap();
            p
        };
        let _a = make("pass2.part-1.jsonl.gz");
        let _b = make("pass2.part-10.jsonl.gz");
        let _c = make("pass2.part-2.jsonl.gz");
        let base = dir.path().join("pass2.jsonl");
        let outs = glob_pass2_outputs(&base).unwrap();
        let names: Vec<String> = outs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "pass2.part-1.jsonl.gz",
                "pass2.part-2.jsonl.gz",
                "pass2.part-10.jsonl.gz",
            ]
        );
    }

    #[test]
    fn glob_pass2_outputs_single_file_compressed() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("pass2.jsonl");
        let gz = dir.path().join("pass2.jsonl.gz");
        std::fs::write(&gz, b"\n").unwrap();
        let outs = glob_pass2_outputs(&base).unwrap();
        let names: Vec<String> = outs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["pass2.jsonl.gz"]);
    }

    #[cfg(windows)]
    #[test]
    fn sh_quote_windows_doubles_quotes() {
        // Use a normal string so that the runtime content has single backslashes
        let s = "C:\\Program Files\\X \"Y\""; // C:\Program Files\X "Y"
        let quoted = sh_quote(s);
        let expected = format!("\"{}\"", s.replace('"', "\"\""));
        assert_eq!(quoted, expected);
    }

    #[cfg(not(windows))]
    #[test]
    fn sh_quote_unix_backslashes_quotes() {
        let s = r#"/tmp/has space/and "quote""#;
        let expected = format!("\"{}\"", s.replace('"', "\\\""));
        assert_eq!(sh_quote(s), expected);
    }
}

fn default_final_manifest_path(final_out: &Path) -> PathBuf {
    let parent = final_out.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}.manifest.json", stem_for_artifacts(final_out)))
}

#[cfg(windows)]
fn sh_quote(s: &str) -> String {
    if s.contains(' ') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(not(windows))]
fn sh_quote(s: &str) -> String {
    if s.contains(' ') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn append_from_child_stdout(mut child: std::process::Child, out_path: &Path) -> Result<()> {
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_path)
        .with_context(|| format!("open append {}", out_path.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut stdout =
        child.stdout.take().ok_or_else(|| anyhow!("failed to capture child stdout"))?;
    loop {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("child exited with status {}", status));
    }
    Ok(())
}

fn count_lines(path: &Path) -> Result<usize> {
    let f = File::open(path)?;
    let mut r = BufReader::new(f);
    let mut buf = [0u8; 64 * 1024];
    let mut cnt = 0usize;
    let mut last: Option<u8> = None;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for b in &buf[..n] {
            if *b == b'\n' {
                cnt += 1;
            }
            last = Some(*b);
        }
    }
    if matches!(last, Some(b) if b != b'\n') {
        cnt += 1;
    }
    Ok(cnt)
}

fn glob_pass2_outputs(base: &Path) -> Result<Vec<PathBuf>> {
    // base: <out-dir>/pass2.jsonl, enumerate:
    //   - pass2.jsonl (single-file)
    //   - pass2.part-*.jsonl[.gz|.zst]
    let mut outs = Vec::new();
    if base.exists() {
        outs.push(base.to_path_buf());
    } else if let (Some(dir), Some(fname)) = (base.parent(), base.file_name()) {
        let fname = fname.to_string_lossy();
        let gz = dir.join(format!("{}.gz", fname));
        if gz.exists() {
            outs.push(gz);
        }
        let zst = dir.join(format!("{}.zst", fname));
        if zst.exists() {
            outs.push(zst);
        }
    }
    if let Some(dir) = base.parent() {
        let stem = base.file_stem().and_then(OsStr::to_str).unwrap_or("pass2");
        for e in fs::read_dir(dir)? {
            let p = e?.path();
            if !p.is_file() {
                continue;
            }
            let fname = p.file_name().and_then(OsStr::to_str).unwrap_or("");
            if fname.starts_with(&format!("{}.part-", stem))
                && (fname.ends_with(".jsonl")
                    || fname.ends_with(".jsonl.gz")
                    || fname.ends_with(".jsonl.zst"))
            {
                outs.push(p);
            }
        }
    }
    // Deduplicate any accidental duplicates, then sort by natural part index if present
    let mut uniq: HashSet<PathBuf> = HashSet::new();
    outs.retain(|p| uniq.insert(p.clone()));

    fn part_index(fname: &str) -> Option<u64> {
        let pat = "part-";
        let i = fname.find(pat)?;
        let rest = &fname[i + pat.len()..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse::<u64>().ok()
    }

    outs.sort_by(|a, b| {
        let fa = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let fb = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
        match (part_index(fa), part_index(fb)) {
            (Some(ia), Some(ib)) => ia.cmp(&ib),
            _ => fa.cmp(fb),
        }
    });
    Ok(outs)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let final_out = cli.final_out.clone();
    let out_dir = cli.out_dir.clone().unwrap_or_else(|| default_out_dir(&final_out));
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir -p {}", out_dir.display()))?;

    let orch_manifest_path = cli
        .orchestrator_manifest_out
        .clone()
        .or(cli.legacy_manifest_out.clone())
        .unwrap_or(out_dir.join("orchestrate_ambiguous.manifest.json"));

    // Step A: extract to temporary file, then normalize+unique to pass2_input.sfens
    let tmp_extract = out_dir.join("pass2_input.tmp");
    let sfens_out = out_dir.join("pass2_input.sfens");
    // remove previous intermediates if present
    let _ = fs::remove_file(&tmp_extract);
    let _ = fs::remove_file(&sfens_out);

    let mut inputs_info: Vec<Value> = Vec::new();
    let extract_bin = find_tool("extract_flagged_positions");

    for p in &cli.pass1 {
        // Record manifest resolution (strict) for provenance
        let mut src_obj = json!({ "path": p.display().to_string() });
        if let Ok(Some(res)) = resolve_manifest(p, AutoloadMode::Strict) {
            src_obj["resolved_manifest_path"] = json!(res.path.display().to_string());
            src_obj["resolved_manifest_scope"] = json!(res.scope);
            src_obj["resolved_manifest_verified"] = json!(res.verified);
            src_obj["resolved_manifest_reason"] = json!(res.reason);
            if let Some(b) = res.output_bytes {
                src_obj["resolved_output_bytes"] = json!(b);
            }
            if let Some(s) = res.output_sha256 {
                src_obj["resolved_output_sha256"] = json!(s);
            }
        }
        inputs_info.push(src_obj);

        if cli.dry_run {
            println!(
                "[dry-run] {} {} - --gap-threshold {}{}{}{}",
                sh_quote(&extract_bin.display().to_string()),
                sh_quote(&p.display().to_string()),
                cli.gap_threshold,
                if cli.include_non_exact {
                    " --include-non-exact"
                } else {
                    ""
                },
                if let Some(n) = cli.include_aspiration_failures {
                    format!(" --include-aspiration-failures {}", n)
                } else {
                    String::new()
                },
                if cli.include_mate_boundary {
                    " --include-mate-boundary"
                } else {
                    ""
                }
            );
            continue;
        }

        let mut cmd = Command::new(&extract_bin);
        cmd.arg(p).arg("-").arg("--gap-threshold").arg(cli.gap_threshold.to_string());
        if cli.include_non_exact {
            cmd.arg("--include-non-exact");
        }
        if let Some(n) = cli.include_aspiration_failures {
            cmd.arg("--include-aspiration-failures").arg(n.to_string());
        }
        if cli.include_mate_boundary {
            cmd.arg("--include-mate-boundary");
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
        let child = cmd
            .spawn()
            .with_context(|| format!("spawn extract_flagged_positions for {}", p.display()))?;
        append_from_child_stdout(child, &tmp_extract)?;
    }

    if cli.dry_run {
        println!("[dry-run] normalize+unique -> {}", sh_quote(&sfens_out.display().to_string()));
    } else {
        // Normalize + unique
        let inp =
            File::open(&tmp_extract).with_context(|| format!("open {}", tmp_extract.display()))?;
        let mut out = BufWriter::new(File::create(&sfens_out)?);
        let mut seen: HashSet<String> = HashSet::new();
        for line in BufReader::new(inp).lines() {
            let l = match line {
                Ok(s) => s,
                Err(_) => continue,
            };
            let s = l.trim();
            if s.is_empty() {
                continue;
            }
            let start = match s.find("sfen ") {
                Some(i) => i + 5,
                None => continue,
            };
            let rest_raw = &s[start..];
            let rest_norm = rest_raw.replace('\t', " ");
            let end = rest_norm
                .find(" moves")
                .or_else(|| rest_norm.find('#'))
                .unwrap_or(rest_norm.len());
            let sfen = rest_norm[..end].trim();
            if sfen.is_empty() {
                continue;
            }
            if let Some(key) = normalize_4t(sfen) {
                if seen.insert(key.clone()) {
                    writeln!(out, "sfen {}", key)?;
                }
            }
        }
        out.flush()?;
    }

    // Compute sfens sha/bytes and counts
    let (sfens_sha, sfens_bytes) = if cli.dry_run {
        (String::from("<dry-run>"), 0u64)
    } else {
        sha256_and_size(&sfens_out)?
    };
    let extracted_count = if cli.dry_run {
        0
    } else {
        count_lines(&sfens_out)?
    };

    if !cli.dry_run && extracted_count == 0 {
        eprintln!("[warn] No positions extracted; skipping re-annotation and merge.");
    }

    // Step B: re-annotate (generate_nnue_training_data)
    let pass2_base = out_dir.join("pass2.jsonl");
    let mut pass2_outputs: Vec<PathBuf> = Vec::new();
    let gen_bin = find_tool("generate_nnue_training_data");
    // Build planned args regardless of dry-run so we can print the plan
    let mut gen_args: Vec<String> = vec![
        sfens_out.display().to_string(),
        pass2_base.display().to_string(),
    ];
    // positional optional args (depth/batch) are not provided to keep CLI simpler
    // flags
    gen_args.push("--engine".into());
    gen_args.push(cli.engine.clone());
    gen_args.push("--output-format".into());
    gen_args.push("jsonl".into());
    gen_args.push("--hash-mb".into());
    gen_args.push(cli.hash_mb.to_string());
    gen_args.push("--multipv".into());
    gen_args.push(cli.multipv.to_string());
    gen_args.push("--teacher-profile".into());
    gen_args.push(cli.teacher_profile.clone());
    gen_args.push("--split".into());
    gen_args.push(cli.split_every.to_string());
    if let Some(md) = cli.min_depth {
        gen_args.push("--min-depth".into());
        gen_args.push(md.to_string());
    } else {
        // Try to infer from pass1 manifests: max(effective_min_depth)+1
        let mut max_eff: Option<u8> = None;
        for p in &cli.pass1 {
            if let Ok(Some(res)) = resolve_manifest(p, AutoloadMode::Strict) {
                if let Some(d) =
                    res.json.get("effective_min_depth").and_then(|x| x.as_u64()).map(|v| v as u8)
                {
                    max_eff = Some(max_eff.map(|m| m.max(d)).unwrap_or(d));
                }
            }
        }
        if let Some(eff) = max_eff.map(|v| v.saturating_add(1)) {
            if cli.verbose {
                eprintln!("[info] inferred --min-depth {} from pass1 manifests", eff);
            }
            gen_args.push("--min-depth".into());
            gen_args.push(eff.to_string());
        }
    }
    if let Some(n) = cli.nodes {
        gen_args.push("--nodes".into());
        gen_args.push(n.to_string());
    }
    if let Some(ms) = cli.time_limit_ms {
        gen_args.push("--time-limit-ms".into());
        gen_args.push(ms.to_string());
    }
    if let Some(j) = cli.jobs {
        gen_args.push("--jobs".into());
        gen_args.push(j.to_string());
    }
    if cli.reuse_tt {
        gen_args.push("--reuse-tt".into());
    }
    if let Some(c) = &cli.compress {
        gen_args.push("--compress".into());
        gen_args.push(c.clone());
    }
    if let Some(p) = &cli.structured_log {
        gen_args.push("--structured-log".into());
        gen_args.push(p.display().to_string());
    }
    if let Some(w) = &cli.nnue_weights {
        gen_args.push("--nnue-weights".into());
        gen_args.push(w.display().to_string());
    }
    if let Some(th) = cli.amb_gap2_threshold {
        gen_args.push("--amb-gap2-threshold".into());
        gen_args.push(th.to_string());
    }
    if cli.amb_allow_inexact {
        gen_args.push("--amb-allow-inexact".into());
    }
    if let Some(m) = &cli.entropy_mate_mode {
        gen_args.push("--entropy-mate-mode".into());
        gen_args.push(m.clone());
    }
    if let Some(s) = cli.entropy_scale {
        gen_args.push("--entropy-scale".into());
        gen_args.push(s.to_string());
    }
    if cli.dry_run {
        let joined = gen_args.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ");
        println!("[dry-run] {} {}", sh_quote(&gen_bin.display().to_string()), joined);
    } else if extracted_count > 0 {
        let status = Command::new(&gen_bin)
            .args(&gen_args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| "run generate_nnue_training_data")?;
        if !status.success() {
            return Err(anyhow!("generate_nnue_training_data failed"));
        }
        pass2_outputs = glob_pass2_outputs(&pass2_base)?;
        if pass2_outputs.is_empty() {
            eprintln!(
                "[warn] pass2 outputs not found under {}; merge will use only pass1 inputs",
                pass2_base
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .display()
            );
        }
    }

    // Resolve pass2 count from aggregated manifest if present
    let mut pass2_count: usize = 0;
    let mut pass2_manifests: Vec<Value> = Vec::new();
    if !cli.dry_run && extracted_count > 0 {
        // Try resolve manifest for base (aggregate) first
        let agg_manifest_path = pass2_base.with_file_name(format!(
            "{}.manifest.json",
            pass2_base.file_stem().and_then(OsStr::to_str).unwrap_or("pass2")
        ));
        if agg_manifest_path.exists() {
            if let Ok(txt) = fs::read_to_string(&agg_manifest_path) {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    if let Some(c) = v.get("count").and_then(|x| x.as_u64()) {
                        pass2_count = c as usize;
                    }
                    pass2_manifests.push(v);
                }
            }
        }
        // Also collect per-part manifests if any
        for (idx, p) in pass2_outputs.iter().enumerate() {
            let stem = p.file_stem().and_then(OsStr::to_str).unwrap_or("").to_string();
            // strip .jsonl if present
            let stem = stem.strip_suffix(".jsonl").unwrap_or(&stem).to_string();
            let man = p
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("{}.manifest.json", stem));
            if man.exists() {
                if let Ok(txt) = fs::read_to_string(&man) {
                    if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                        pass2_manifests.push(v);
                    }
                }
            } else {
                let _ = idx; // silence
            }
        }
        // Fallback: if aggregate count missing, sum per-part counts
        if pass2_count == 0 {
            let mut sum = 0usize;
            for v in &pass2_manifests {
                if let Some(c) = v.get("count_in_part").and_then(|x| x.as_u64()) {
                    sum += c as usize;
                } else if let Some(c) = v.get("count").and_then(|x| x.as_u64()) {
                    sum += c as usize;
                }
            }
            pass2_count = sum;
        }
    }

    // Step C: merge pass1 + pass2 into final
    let final_manifest_path = cli
        .final_manifest_out
        .clone()
        .unwrap_or_else(|| default_final_manifest_path(&cli.final_out));

    let mut final_written: usize = 0;
    if cli.dry_run {
        let merge_bin = find_tool("merge_annotation_results");
        let mut margs: Vec<String> = vec![
            "--dedup-by-sfen".into(),
            "--mode".into(),
            cli.merge_mode.clone(),
            "--manifest-out".into(),
            final_manifest_path.display().to_string(),
        ];
        for p in &cli.pass1 {
            margs.push(p.display().to_string());
        }
        // In dry-run, we don't know actual parts; show base path
        margs.push(pass2_base.display().to_string());
        margs.push(cli.final_out.display().to_string());
        let joined = margs.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ");
        println!("[dry-run] {} {}", sh_quote(&merge_bin.display().to_string()), joined);
    } else if extracted_count > 0 {
        let merge_bin = find_tool("merge_annotation_results");
        let mut args: Vec<String> = vec![
            "--dedup-by-sfen".into(),
            "--mode".into(),
            cli.merge_mode.clone(), // depth-first expected
            "--manifest-out".into(),
            final_manifest_path.display().to_string(),
        ];
        for p in &cli.pass1 {
            args.push(p.display().to_string());
        }
        for p in &pass2_outputs {
            args.push(p.display().to_string());
        }
        args.push(cli.final_out.display().to_string());
        let status = Command::new(&merge_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| "run merge_annotation_results")?;
        if !status.success() {
            return Err(anyhow!("merge_annotation_results failed"));
        }
        // Read final manifest to get written_lines
        if let Ok(txt) = fs::read_to_string(&final_manifest_path) {
            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                if let Some(w) = v
                    .get("aggregated")
                    .and_then(|a| a.get("written_lines"))
                    .and_then(|x| x.as_u64())
                {
                    final_written = w as usize;
                }
            }
        }
    }

    // Step D: analyze (optional)
    let mut analyze_info: Option<Value> = None;
    if cli.analyze_summary {
        let analyze_bin = find_tool("analyze_teaching_quality");
        let analyze_out = cli.analyze_out.clone().unwrap_or(out_dir.join("quality.json"));
        // Input to analyze: prefer final when we completed merge; else fall back to first pass1
        let analyze_input = if extracted_count > 0 {
            cli.final_out.display().to_string()
        } else if let Some(first) = cli.pass1.first() {
            first.display().to_string()
        } else {
            String::new()
        };
        if extracted_count == 0 {
            eprintln!(
                "[info] analyze input = {} (no extracted positions; fallback to pass1[0])",
                analyze_input
            );
        }
        // Prefer multipv from pass2 manifest when available; else use CLI; if no extraction, keep CLI
        let mut expected_mpv = if extracted_count > 0 {
            pass2_manifests
                .iter()
                .filter_map(|m| m.get("multipv").and_then(|x| x.as_u64()))
                .map(|v| v as usize)
                .next()
                .unwrap_or(cli.multipv as usize)
        } else {
            cli.multipv as usize
        };
        // If final manifest has aggregated.multipv, prefer it
        if !cli.dry_run && final_manifest_path.exists() {
            if let Ok(txt) = fs::read_to_string(&final_manifest_path) {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    if let Some(mv) =
                        v.get("aggregated").and_then(|a| a.get("multipv")).and_then(|x| x.as_u64())
                    {
                        expected_mpv = mv as usize;
                    }
                }
            }
        }
        if cli.dry_run {
            println!(
                "[dry-run] {} {} --json --expected-multipv {} --manifest-autoload-mode strict > {}",
                sh_quote(&analyze_bin.display().to_string()),
                sh_quote(&analyze_input),
                expected_mpv,
                sh_quote(&analyze_out.display().to_string())
            );
        } else {
            // 1) JSON -> file
            let mut child = Command::new(&analyze_bin)
                .arg(&analyze_input)
                .arg("--json")
                .arg("--expected-multipv")
                .arg(expected_mpv.to_string())
                .arg("--manifest-autoload-mode")
                .arg("strict")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .with_context(|| "spawn analyze_teaching_quality --json")?;
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                so.read_to_string(&mut out)?;
            }
            let _ = child.wait()?;
            if out.trim().is_empty() {
                eprintln!("[warn] analyze_teaching_quality produced no JSON output");
            } else {
                write_atomic(&analyze_out, &out)?;
            }
            // 2) Human summary -> console
            let _ = Command::new(&analyze_bin)
                .arg(&analyze_input)
                .arg("--summary")
                .arg("--expected-multipv")
                .arg(expected_mpv.to_string())
                .arg("--manifest-autoload-mode")
                .arg("strict")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            analyze_info = Some(json!({
                "summary_json": analyze_out.display().to_string(),
                "expected_mpv": expected_mpv,
            }));
        }
    }

    // Orchestration manifest
    let orch = json!({
        "tool": "orchestrate_ambiguous",
        "generated_at": Utc::now().to_rfc3339(),
        "env": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "inputs": inputs_info,
        "extract": {
            "opts": {
                "gap_threshold": cli.gap_threshold,
                "include_non_exact": cli.include_non_exact,
                "include_aspiration_failures": cli.include_aspiration_failures,
                "include_mate_boundary": cli.include_mate_boundary,
            },
            "sfens": { "path": sfens_out.display().to_string(), "sha256": sfens_sha, "bytes": sfens_bytes },
            "extracted_count": extracted_count,
        },
        "reannotate": if extracted_count>0 { json!({
            "base": pass2_base.display().to_string(),
            "outputs": pass2_outputs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "opts": {
                "engine": cli.engine,
                "nnue_weights": cli.nnue_weights.as_ref().map(|p| p.display().to_string()),
                "teacher_profile": cli.teacher_profile,
                "multipv": cli.multipv,
                "min_depth": cli.min_depth,
                "nodes": cli.nodes,
                "time_limit_ms": cli.time_limit_ms,
                "jobs": cli.jobs,
                "hash_mb": cli.hash_mb,
                "reuse_tt": cli.reuse_tt,
                "split_every": cli.split_every,
                "compress": cli.compress,
                "structured_log": cli.structured_log.as_ref().map(|p| p.display().to_string()),
                "amb_gap2_threshold": cli.amb_gap2_threshold,
                "amb_allow_inexact": cli.amb_allow_inexact,
                "entropy_mate_mode": cli.entropy_mate_mode,
                "entropy_scale": cli.entropy_scale,
            },
            "manifests": pass2_manifests,
            "pass2_generated": pass2_count,
        }) } else { Value::Null },
        "merge": if extracted_count>0 { json!({
            "mode": cli.merge_mode,
            "inputs": cli.pass1.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "final": cli.final_out.display().to_string(),
            "manifest_out": final_manifest_path.display().to_string(),
            "final_written": final_written,
        }) } else { Value::Null },
        "analyze": analyze_info,
        "counts": {
            "extracted": extracted_count,
            "pass2_generated": pass2_count,
            "final_written": final_written,
        }
    });

    if cli.dry_run {
        println!(
            "[dry-run] would write orchestration manifest to {}",
            sh_quote(&orch_manifest_path.display().to_string())
        );
    } else {
        write_atomic(&orch_manifest_path, &serde_json::to_string_pretty(&orch)?)?;
        if cli.verbose {
            eprintln!("orchestration manifest: {}", orch_manifest_path.display());
        }
    }

    // Optionally prune intermediates (not implemented yet; keep by default)
    if !cli.keep_intermediate {
        // Placeholder: leave intermediates for now
    }

    Ok(())
}
