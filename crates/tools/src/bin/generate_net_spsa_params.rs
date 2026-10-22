use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rshogi_core::nnue::net_bin_layout::LayerStacksBinLayout;
use rshogi_core::nnue::{NetCoefficientId, NetTensorKind};
use sha2::{Digest, Sha256};
use tools::output_path::ensure_safe_output_path;
use tools::spsa_param_mapping::SPSA_PARAMS_FORMAT_HEADER;

const DEFAULT_MAX_PARAMS: usize = 4096;
const R_END: &str = "0.002";
const KIND_ORDER: [NetTensorKind; 4] = [
    NetTensorKind::OutputWeight,
    NetTensorKind::OutputBias,
    NetTensorKind::FtBias,
    NetTensorKind::L2Weight,
];

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "LayerStacks NNUE から net 重み SPSA 用 .params を生成する"
)]
struct Cli {
    /// 入力 LayerStacks NNUE `.bin`
    #[arg(long)]
    nnue: PathBuf,
    /// 出力先 `.params`
    #[arg(long)]
    output: PathBuf,
    /// 対象 kind の comma 区切り
    #[arg(long, default_value = "out_w,out_b")]
    targets: String,
    /// 係数選択: all / zero / abs-below=<T>
    #[arg(long, default_value = "all")]
    select: String,
    /// kind ごとの delta 範囲上書き (`<kind>=<N>` で ±N)
    #[arg(long = "range")]
    ranges: Vec<String>,
    /// kind ごとの終端摂動幅上書き (`<kind>=<C>`)
    #[arg(long = "c-end")]
    c_ends: Vec<String>,
    /// 出力 parameter 数の上限
    #[arg(long, default_value_t = DEFAULT_MAX_PARAMS)]
    max_params: usize,
}

#[derive(Debug, Clone, Copy)]
enum Selection {
    All,
    Zero,
    AbsBelow(i64),
}

impl Selection {
    fn includes(self, value: i32) -> bool {
        match self {
            Self::All => true,
            Self::Zero => value == 0,
            Self::AbsBelow(threshold) => i64::from(value).abs() < threshold,
        }
    }
}

#[derive(Debug)]
struct GenerateConfig {
    targets: BTreeSet<NetTensorKind>,
    selection: Selection,
    ranges: BTreeMap<NetTensorKind, i32>,
    c_ends: BTreeMap<NetTensorKind, i32>,
    max_params: usize,
}

impl GenerateConfig {
    fn from_cli(cli: &Cli) -> Result<Self> {
        if cli.max_params == 0 {
            bail!("--max-params must be at least 1");
        }
        Ok(Self {
            targets: parse_targets(&cli.targets)?,
            selection: parse_selection(&cli.select)?,
            ranges: parse_kind_values(&cli.ranges, "--range")?,
            c_ends: parse_kind_values(&cli.c_ends, "--c-end")?,
            max_params: cli.max_params,
        })
    }

    fn range(&self, kind: NetTensorKind) -> i32 {
        self.ranges.get(&kind).copied().unwrap_or_else(|| defaults(kind).0)
    }

    fn c_end(&self, kind: NetTensorKind) -> i32 {
        self.c_ends.get(&kind).copied().unwrap_or_else(|| defaults(kind).1)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli)
}

fn run(cli: &Cli) -> Result<()> {
    let config = GenerateConfig::from_cli(cli)?;
    ensure_safe_output_path(&cli.output, &cli.nnue)?;
    let mut input = BufReader::new(
        File::open(&cli.nnue).with_context(|| format!("failed to open {}", cli.nnue.display()))?,
    );
    let net_name = cli
        .nnue
        .file_name()
        .and_then(|name| name.to_str())
        .context("--nnue must have a UTF-8 basename")?;
    let output = generate_params(&mut input, net_name, &config)?;
    write_output(&cli.output, output.as_bytes())
}

fn generate_params<R: Read + Seek>(
    reader: &mut R,
    net_name: &str,
    config: &GenerateConfig,
) -> Result<String> {
    let layout = LayerStacksBinLayout::from_reader(reader).context("invalid LayerStacks NNUE")?;
    reader.rewind().context("failed to rewind NNUE for SHA-256")?;
    let mut hasher = Sha256::new();
    std::io::copy(reader, &mut hasher).context("failed to hash NNUE")?;
    let sha256 = format!("{:x}", hasher.finalize());
    let mut output = format!(
        "# net={net_name} sha256={sha256} arch={} buckets={}\n{SPSA_PARAMS_FORMAT_HEADER}\n",
        layout.architecture, layout.num_buckets
    );
    let mut count = 0usize;
    for kind in KIND_ORDER {
        if !config.targets.contains(&kind) {
            continue;
        }
        match kind {
            NetTensorKind::FtBias => {
                for index in 0..layout.tensor_shape(kind).element_count {
                    emit_parameter(
                        &mut output,
                        &mut count,
                        &layout,
                        NetCoefficientId {
                            kind,
                            bucket: None,
                            index,
                        },
                        config,
                    )?;
                }
            }
            NetTensorKind::OutputBias => {
                for bucket in 0..layout.num_buckets {
                    emit_parameter(
                        &mut output,
                        &mut count,
                        &layout,
                        NetCoefficientId {
                            kind,
                            bucket: Some(bucket),
                            index: 0,
                        },
                        config,
                    )?;
                }
            }
            NetTensorKind::OutputWeight | NetTensorKind::L2Weight => {
                let input_dim = if kind == NetTensorKind::OutputWeight {
                    layout.l3
                } else {
                    2 * (layout.l2 - 1)
                };
                let padded_input = input_dim.div_ceil(32) * 32;
                let element_count = layout.tensor_shape(kind).element_count;
                for bucket in 0..layout.num_buckets {
                    for index in 0..element_count {
                        if index % padded_input >= input_dim {
                            continue;
                        }
                        emit_parameter(
                            &mut output,
                            &mut count,
                            &layout,
                            NetCoefficientId {
                                kind,
                                bucket: Some(bucket),
                                index,
                            },
                            config,
                        )?;
                    }
                }
            }
        }
    }
    Ok(output)
}

fn emit_parameter(
    output: &mut String,
    count: &mut usize,
    layout: &LayerStacksBinLayout,
    id: NetCoefficientId,
    config: &GenerateConfig,
) -> Result<()> {
    let base = layout.coefficient(&id)?;
    if !config.selection.includes(base) {
        return Ok(());
    }
    *count += 1;
    if *count > config.max_params {
        bail!("selected parameter count exceeds --max-params {}", config.max_params);
    }
    let range = config.range(id.kind);
    let c_end = config.c_end(id.kind);
    output.push_str(&format!(
        "{},int,0,{},{},{c_end},{R_END} // base={base}\n",
        id.usi_name(),
        -range,
        range
    ));
    Ok(())
}

fn parse_targets(value: &str) -> Result<BTreeSet<NetTensorKind>> {
    let mut targets = BTreeSet::new();
    for token in value.split(',') {
        if token.is_empty() {
            bail!("--targets contains an empty token");
        }
        targets.insert(parse_kind(token)?);
    }
    if targets.is_empty() {
        bail!("--targets must not be empty");
    }
    Ok(targets)
}

fn parse_selection(value: &str) -> Result<Selection> {
    match value {
        "all" => Ok(Selection::All),
        "zero" => Ok(Selection::Zero),
        _ => {
            let threshold = value
                .strip_prefix("abs-below=")
                .with_context(|| format!("unknown --select value: {value}"))?
                .parse::<i64>()
                .with_context(|| format!("invalid abs-below threshold: {value}"))?;
            if threshold < 0 {
                bail!("abs-below threshold must be non-negative");
            }
            Ok(Selection::AbsBelow(threshold))
        }
    }
}

fn parse_kind_values(values: &[String], option_name: &str) -> Result<BTreeMap<NetTensorKind, i32>> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let (kind, number) = value
            .split_once('=')
            .with_context(|| format!("{option_name} expects <kind>=<positive integer>: {value}"))?;
        let kind = parse_kind(kind)?;
        let number = number
            .parse::<i32>()
            .with_context(|| format!("invalid {option_name} value: {value}"))?;
        if number <= 0 {
            bail!("{option_name} value must be positive: {value}");
        }
        if parsed.insert(kind, number).is_some() {
            bail!("duplicate {option_name} override for {}", kind.token());
        }
    }
    Ok(parsed)
}

fn parse_kind(token: &str) -> Result<NetTensorKind> {
    match token {
        "out_w" => Ok(NetTensorKind::OutputWeight),
        "out_b" => Ok(NetTensorKind::OutputBias),
        "ft_b" => Ok(NetTensorKind::FtBias),
        "l2_w" => Ok(NetTensorKind::L2Weight),
        _ => bail!("unknown net tensor kind: {token}"),
    }
}

fn defaults(kind: NetTensorKind) -> (i32, i32) {
    match kind {
        NetTensorKind::OutputWeight | NetTensorKind::L2Weight => (24, 2),
        NetTensorKind::OutputBias => (512, 32),
        NetTensorKind::FtBias => (48, 4),
    }
}

fn write_output(path: &Path, contents: &[u8]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(contents)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::nnue::features::{FeatureSet, HalfKPFeatureSet};
    use rshogi_core::nnue::net_delta::test_utils::{
        SyntheticFtEncoding, build_synthetic_layer_stacks_with_ft_encoding,
    };
    use std::io::Cursor;
    use tools::spsa_param_mapping::parse_param_line;

    fn config(selection: Selection) -> GenerateConfig {
        GenerateConfig {
            targets: KIND_ORDER.into_iter().collect(),
            selection,
            ranges: BTreeMap::new(),
            c_ends: BTreeMap::new(),
            max_params: 4096,
        }
    }

    fn rows(contents: &str) -> Vec<tools::spsa_param_mapping::RawParamRow> {
        contents
            .lines()
            .enumerate()
            .filter_map(|(index, line)| parse_param_line(line, index + 1).expect("params line"))
            .collect()
    }

    fn base(row: &tools::spsa_param_mapping::RawParamRow) -> i32 {
        row.comment
            .strip_prefix("base=")
            .expect("base comment")
            .parse()
            .expect("base value")
    }

    #[test]
    fn generated_rows_round_trip_and_skip_padding() {
        // WHY: tools の固定 1536x16x32 loader では小型合成 net を読めないため、ここでは layout の
        // 往復を検証し、layout と dynamic network の一致は core の対応テストに委ねる。
        for (buckets, encoding) in [
            (2, SyntheticFtEncoding::Leb128Combined),
            (3, SyntheticFtEncoding::Leb128Split),
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                HalfKPFeatureSet::DIMENSIONS,
                32,
                4,
                3,
                buckets,
                encoding,
            );
            let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
            let contents = generate_params(
                &mut Cursor::new(&synthetic.bytes),
                "test.bin",
                &config(Selection::All),
            )
            .expect("params");
            let parsed = rows(&contents);
            assert!(!parsed.is_empty());
            for row in parsed {
                let id = NetCoefficientId::parse_usi_name(&row.name).expect("USI option name");
                assert_eq!(row.value_text, "0");
                assert_eq!(row.col6_text, R_END);
                let (range, c_end) = defaults(id.kind);
                assert_eq!(row.min_text, (-range).to_string());
                assert_eq!(row.max_text, range.to_string());
                assert_eq!(row.col5_text, c_end.to_string());
                assert_eq!(layout.coefficient(&id).expect("coefficient"), base(&row));
                match id.kind {
                    NetTensorKind::OutputWeight => assert!(id.index % 32 < 3),
                    NetTensorKind::L2Weight => assert!(id.index % 32 < 6),
                    NetTensorKind::OutputBias => assert_eq!(id.index, 0),
                    NetTensorKind::FtBias => {}
                }
            }
        }
    }

    #[test]
    fn selection_filters_by_current_value() {
        let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            HalfKPFeatureSet::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        synthetic.bytes[layout.feature_transformer.biases.start] = 0;
        synthetic.bytes[layout.buckets[0].l2.weights.start] = 0;
        synthetic.bytes[layout.buckets[0].output.biases.clone()]
            .copy_from_slice(&0i32.to_le_bytes());
        synthetic.bytes[layout.buckets[0].output.weights.start] = 0;

        let zero = generate_params(
            &mut Cursor::new(&synthetic.bytes),
            "test.bin",
            &config(Selection::Zero),
        )
        .expect("zero params");
        let zero_rows = rows(&zero);
        assert_eq!(zero_rows.len(), 4);
        assert!(zero_rows.iter().all(|row| base(row) == 0));

        let below = generate_params(
            &mut Cursor::new(&synthetic.bytes),
            "test.bin",
            &config(Selection::AbsBelow(3)),
        )
        .expect("abs-below params");
        let below_rows = rows(&below);
        assert!(below_rows.len() > zero_rows.len());
        assert!(below_rows.iter().all(|row| i64::from(base(row)).abs() < 3));
    }

    #[test]
    fn output_is_deterministic_and_limit_is_fail_closed() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            HalfKPFeatureSet::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Combined,
        );
        let first = generate_params(
            &mut Cursor::new(&synthetic.bytes),
            "same.bin",
            &config(Selection::All),
        )
        .expect("first");
        let second = generate_params(
            &mut Cursor::new(&synthetic.bytes),
            "same.bin",
            &config(Selection::All),
        )
        .expect("second");
        assert_eq!(first.as_bytes(), second.as_bytes());

        let mut limited = config(Selection::All);
        limited.max_params = 1;
        assert!(generate_params(&mut Cursor::new(&synthetic.bytes), "same.bin", &limited).is_err());
    }

    #[test]
    fn rejects_unknown_target_and_applies_overrides() {
        assert!(parse_targets("out_w,unknown").is_err());
        let ranges = parse_kind_values(&["out_w=7".to_owned()], "--range").expect("range");
        let c_ends = parse_kind_values(&["out_w=3".to_owned()], "--c-end").expect("c-end");
        assert_eq!(ranges[&NetTensorKind::OutputWeight], 7);
        assert_eq!(c_ends[&NetTensorKind::OutputWeight], 3);
    }

    fn cli(nnue: PathBuf, output: PathBuf) -> Cli {
        Cli {
            nnue,
            output,
            targets: "out_b".to_owned(),
            select: "all".to_owned(),
            ranges: Vec::new(),
            c_ends: Vec::new(),
            max_params: DEFAULT_MAX_PARAMS,
        }
    }

    #[test]
    fn same_input_and_output_is_rejected_without_truncation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("net.bin");
        let original = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            HalfKPFeatureSet::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Combined,
        )
        .bytes;
        std::fs::write(&path, &original)?;

        assert!(run(&cli(path.clone(), path.clone())).is_err());
        assert_eq!(std::fs::read(path)?, original);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_output_is_rejected_without_truncating_input() -> Result<()> {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir()?;
        let input = dir.path().join("net.bin");
        let output = dir.path().join("output.params");
        let original = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            HalfKPFeatureSet::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        )
        .bytes;
        std::fs::write(&input, &original)?;
        symlink(&input, &output)?;

        assert!(run(&cli(input.clone(), output)).is_err());
        assert_eq!(std::fs::read(input)?, original);
        Ok(())
    }

    #[test]
    fn hardlink_output_is_rejected_without_truncating_input() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("net.bin");
        let output = dir.path().join("output.params");
        let original = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            HalfKPFeatureSet::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Combined,
        )
        .bytes;
        std::fs::write(&input, &original)?;
        std::fs::hard_link(&input, &output)?;

        assert!(run(&cli(input.clone(), output)).is_err());
        assert_eq!(std::fs::read(input)?, original);
        Ok(())
    }
}
