use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rshogi_core::nnue::net_bin_layout::{LayerStacksBinLayout, apply_deltas};
use rshogi_core::nnue::net_delta::{add_i8_delta, add_i16_delta, add_i32_delta};
use rshogi_core::nnue::{NetCoefficientId, NetDelta, NetTensorKind};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tools::output_path::{
    ensure_created_paths_distinct, ensure_distinct_output_paths, ensure_safe_output_path,
};
use tools::spsa_param_mapping::{parse_param_line, register_unique_parameter};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "SPSA で確定した delta を LayerStacks NNUE へ焼き込む"
)]
struct Cli {
    /// 入力 LayerStacks NNUE `.bin`
    #[arg(long)]
    nnue: PathBuf,
    /// SPSA 終了後の `.params`
    #[arg(long)]
    params: PathBuf,
    /// 焼き込み後の NNUE `.bin`
    #[arg(long)]
    output: PathBuf,
    /// generator の metadata から控えた入力 NNUE の SHA-256
    #[arg(long, value_name = "HEX")]
    expected_net_sha256: Option<String>,
    /// 入力 SHA-256 の照合元欠落または不一致を許可する
    #[arg(long)]
    allow_net_mismatch: bool,
    /// 適用結果を JSON でも保存する
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct SourcePaths<'a> {
    nnue: &'a Path,
    params: &'a Path,
}

#[derive(Debug, Serialize)]
struct ApplyReport<'a> {
    applied_count: usize,
    clamped_count: usize,
    nonzero_delta_count: usize,
    input_sha256: &'a str,
    output_sha256: &'a str,
    source: SourcePaths<'a>,
}

fn main() -> Result<()> {
    run(&Cli::parse())
}

fn run(cli: &Cli) -> Result<()> {
    reject_path_collisions(cli)?;
    let params = std::fs::read_to_string(&cli.params)
        .with_context(|| format!("failed to read {}", cli.params.display()))?;
    let mut input =
        File::open(&cli.nnue).with_context(|| format!("failed to open {}", cli.nnue.display()))?;
    let input_sha256 = sha256_reader(&mut input, &cli.nnue)?;
    validate_source_hash(
        &params,
        &input_sha256,
        cli.expected_net_sha256.as_deref(),
        cli.allow_net_mismatch,
    )?;
    let deltas = parse_deltas(&params)?;

    // 入力パスが途中で差し替わっても hash・期待値・patch の由来を同じ inode に固定する。
    input.rewind().context("failed to rewind input NNUE after hashing")?;
    let input_layout = LayerStacksBinLayout::from_reader(&mut input)
        .context("failed to parse input NNUE layout")?;
    let expected = expected_coefficients(&input_layout, &deltas)?;
    input.rewind().context("failed to rewind input NNUE before patching")?;
    let mut staged_output = create_staged_file(&cli.output)?;
    let staged_output_path = staged_output.path().to_owned();
    let patch_report = apply_deltas(&mut input, staged_output.as_file_mut(), &deltas)
        .context("failed to patch NNUE stream")?;
    staged_output
        .as_file_mut()
        .flush()
        .with_context(|| format!("failed to flush {}", staged_output_path.display()))?;
    verify_output(&staged_output_path, &expected)?;

    let output_sha256 = sha256_file(&staged_output_path)?;
    let nonzero_delta_count = deltas.iter().filter(|delta| delta.delta != 0).count();
    let staged_report = if let Some(path) = &cli.report {
        let report = ApplyReport {
            applied_count: patch_report.applied,
            clamped_count: patch_report.clamped,
            nonzero_delta_count,
            input_sha256: &input_sha256,
            output_sha256: &output_sha256,
            source: SourcePaths {
                nnue: &cli.nnue,
                params: &cli.params,
            },
        };
        let mut staged = create_staged_file(path)?;
        let staged_path = staged.path().to_owned();
        write_json(staged.as_file_mut(), &staged_path, &report)?;
        Some((staged, path))
    } else {
        None
    };
    // report は net の由来を記録するものなので、net が確定してから書く (net 無しの report を残さない)。
    staged_output
        .persist(&cli.output)
        .with_context(|| format!("failed to rename staged NNUE to {}", cli.output.display()))?;
    if let Some((staged, path)) = staged_report {
        ensure_report_destination_distinct(&cli.output, path)?;
        staged
            .persist(path)
            .with_context(|| format!("failed to rename staged report to {}", path.display()))?;
    }
    println!(
        "applied={} nonzero={} clamped={} output_sha256={output_sha256}",
        patch_report.applied, nonzero_delta_count, patch_report.clamped
    );
    Ok(())
}

fn ensure_report_destination_distinct(output: &Path, report: &Path) -> Result<()> {
    ensure_created_paths_distinct(&[output, report])
        .context("output and report destinations alias after finalizing the NNUE")
}

fn validate_source_hash(
    contents: &str,
    actual: &str,
    expected_option: Option<&str>,
    allow_mismatch: bool,
) -> Result<()> {
    if contents.is_empty() {
        bail!(".params is empty");
    }
    let mut metadata_hash = None;
    for metadata in contents.lines().filter_map(|line| line.strip_prefix('#')) {
        for value in metadata
            .split_ascii_whitespace()
            .filter_map(|field| field.strip_prefix("sha256="))
        {
            validate_sha256(value, ".params metadata")?;
            if metadata_hash.is_some_and(|prior: &str| !prior.eq_ignore_ascii_case(value)) {
                bail!(".params contains conflicting sha256 metadata");
            }
            metadata_hash = Some(value);
        }
    }
    if let Some(expected) = expected_option {
        validate_sha256(expected, "--expected-net-sha256")?;
    }
    if let (Some(metadata), Some(expected)) = (metadata_hash, expected_option)
        && !metadata.eq_ignore_ascii_case(expected)
    {
        bail!(
            "--expected-net-sha256 does not match .params metadata: option={expected}, metadata={metadata}"
        );
    }
    let expected = metadata_hash.or(expected_option);
    let Some(expected) = expected else {
        if allow_mismatch {
            return Ok(());
        }
        bail!(
            ".params has no sha256 metadata; pass --expected-net-sha256 <hex> or --allow-net-mismatch"
        );
    };
    if !expected.eq_ignore_ascii_case(actual) && !allow_mismatch {
        bail!("input NNUE sha256 mismatch: expected={expected}, actual={actual}");
    }
    Ok(())
}

fn validate_sha256(value: &str, source: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{source} has invalid sha256: {value}");
    }
    Ok(())
}

fn parse_deltas(contents: &str) -> Result<Vec<NetDelta>> {
    let mut deltas = Vec::new();
    let mut names = std::collections::HashMap::new();
    for (index, line) in contents.lines().enumerate() {
        let line_no = index + 1;
        let Some(row) = parse_param_line(line, line_no)? else {
            continue;
        };
        register_unique_parameter(&mut names, &row.name, line_no)?;
        if row.not_used {
            continue;
        }
        if row.kind != "int" {
            bail!("line {line_no}: expected parameter kind int, got {}", row.kind);
        }
        let id = NetCoefficientId::parse_usi_name(&row.name)
            .with_context(|| format!("line {line_no}: not a SPSA_NET parameter: {}", row.name))?;
        let value = row
            .value_text
            .parse::<f64>()
            .with_context(|| format!("line {line_no}: invalid delta: {}", row.value_text))?;
        if !value.is_finite() {
            bail!("line {line_no}: delta must be finite: {}", row.value_text);
        }
        let rounded = value.round();
        if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
            bail!("line {line_no}: rounded delta is outside i32 range: {}", row.value_text);
        }
        deltas.push(NetDelta {
            id,
            delta: rounded as i32,
        });
    }
    Ok(deltas)
}

fn expected_coefficients(
    input_layout: &LayerStacksBinLayout,
    deltas: &[NetDelta],
) -> Result<BTreeMap<NetCoefficientId, i32>> {
    let mut expected = BTreeMap::<NetCoefficientId, i32>::new();
    for delta in deltas {
        let current = match expected.get(&delta.id) {
            Some(value) => *value,
            None => input_layout.coefficient(&delta.id)?,
        };
        let value = match delta.id.kind {
            NetTensorKind::OutputWeight | NetTensorKind::L2Weight => {
                let current = i8::try_from(current)
                    .with_context(|| format!("{} is outside i8 range", delta.id.usi_name()))?;
                i32::from(add_i8_delta(current, delta.delta).0)
            }
            NetTensorKind::OutputBias => add_i32_delta(current, delta.delta).0,
            NetTensorKind::FtBias => {
                let current = i16::try_from(current)
                    .with_context(|| format!("{} is outside i16 range", delta.id.usi_name()))?;
                i32::from(add_i16_delta(current, delta.delta).0)
            }
        };
        expected.insert(delta.id.clone(), value);
    }
    Ok(expected)
}

fn verify_output(path: &Path, expected: &BTreeMap<NetCoefficientId, i32>) -> Result<()> {
    let mut output = File::open(path)
        .with_context(|| format!("failed to reopen staged NNUE {}", path.display()))?;
    let output_layout = LayerStacksBinLayout::from_reader(&mut output)
        .context("failed to parse patched NNUE layout")?;
    for (id, expected_value) in expected {
        let actual_value = output_layout.coefficient(id)?;
        if actual_value != *expected_value {
            bail!(
                "patched coefficient mismatch for {}: expected={expected_value}, actual={actual_value}",
                id.usi_name()
            );
        }
    }
    Ok(())
}

fn reject_path_collisions(cli: &Cli) -> Result<()> {
    ensure_safe_output_path(&cli.output, &cli.nnue)?;
    ensure_safe_output_path(&cli.output, &cli.params)?;
    if let Some(report) = &cli.report {
        ensure_safe_output_path(report, &cli.nnue)?;
        ensure_safe_output_path(report, &cli.params)?;
        ensure_distinct_output_paths(&cli.output, report)?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    sha256_reader(&mut file, path)
}

fn sha256_reader(file: &mut File, path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    io::copy(file, &mut digest).with_context(|| format!("failed to hash {}", path.display()))?;
    Ok(format!("{:x}", digest.finalize()))
}

fn create_staged_file(destination: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tempfile::Builder::new()
        .prefix(".apply_net_spsa_params_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create staged file under {}", parent.display()))
}

fn write_json(file: &mut File, path: &Path, report: &ApplyReport<'_>) -> Result<()> {
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report)
        .with_context(|| format!("failed to write {}", path.display()))?;
    writer
        .write_all(b"\n")
        .with_context(|| format!("failed to write {}", path.display()))?;
    writer.flush().with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn duplicate_net_ids_are_rejected_before_finalizing() {
        for second in ["SPSA_NET_ft_b_1", "SPSA_NET_ft_b_01"] {
            let rows = format!(
                "# header\nSPSA_NET_ft_b_1,int,1,-10,10,1,0.002\n{second},int,2,-10,10,1,0.002\n"
            );
            let error = parse_deltas(&rows).unwrap_err().to_string();
            assert!(error.contains("line 3") && error.contains("line 2"), "{error}");
        }
        let rows = "SPSA_NET_ft_b_1,int,1,-10,10,1,0.002\nSPSA_NET_ft_b_2,int,2,-10,10,1,0.002\n";
        let deltas = parse_deltas(rows).unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].delta, 1);
        assert_eq!(deltas[1].delta, 2);
    }

    use super::*;

    #[test]
    fn parses_rounded_values_and_skips_not_used_rows() {
        let rows = "# metadata\n\
SPSA_NET_out_w_b0_0,int,1.500000,-3,3,1,0.002\n\
SPSA_NET_ft_b_2,int,-1.500000,-3,3,1,0.002 [[NOT USED]]\n";
        let parsed = parse_deltas(rows).expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].delta, 2);
    }

    #[test]
    fn rejects_non_net_rows_and_non_finite_values() {
        let search = "SPSA_FUTILITY_MARGIN_BASE,int,1,-3,3,1,0.002";
        assert!(parse_deltas(search).is_err());
        let nan = "SPSA_NET_ft_b_0,int,NaN,-3,3,1,0.002";
        assert!(parse_deltas(nan).is_err());
    }

    #[test]
    fn source_hash_uses_metadata_expected_option_or_explicit_override() {
        let actual = "a".repeat(64);
        let other = "b".repeat(64);
        let metadata = format!("# net=x.bin sha256={other} arch=x buckets=2\n");
        assert!(validate_source_hash(&metadata, &actual, None, false).is_err());
        validate_source_hash(&metadata, &actual, None, true).expect("allowed mismatch");
        validate_source_hash("SPSA_NET_ft_b_0,int,0,0,0,1,1", &actual, Some(&actual), false)
            .expect("expected option");
        validate_source_hash("SPSA_NET_ft_b_0,int,0,0,0,1,1", &actual, None, true)
            .expect("explicit override");
        assert!(validate_source_hash(&metadata, &actual, Some(&"c".repeat(64)), true).is_err());
        let error = validate_source_hash("SPSA_NET_ft_b_0,int,0,0,0,1,1", &actual, None, false)
            .expect_err("missing source hash");
        let message = error.to_string();
        assert!(message.contains("--expected-net-sha256"));
        assert!(message.contains("--allow-net-mismatch"));
    }

    #[test]
    fn rejects_report_alias_created_after_preflight() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("tuned.bin");
        let report = dir.path().join("report.json");
        ensure_distinct_output_paths(&output, &report)?;

        std::fs::write(&output, [0u8])?;
        std::fs::hard_link(&output, &report)?;

        assert!(ensure_report_destination_distinct(&output, &report).is_err());
        Ok(())
    }
}
