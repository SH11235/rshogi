//! YaneuraOu ⇔ rshogi SPSA パラメータマッピング表のビルダ
//!
//! 正本ペア（`tune/suisho10.params` と `spsa_params/suisho10_converted.params`）から
//! 値一致（必要に応じ符号反転）で自動的にマッピング候補を抽出し、TOML を出力する。
//! 一意に解決できないケースは `ambiguous` として書き出すので、人手でレビューする。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use rshogi_core::search::SearchTuneParams;

const NOT_USED_MARKER: &str = "[[NOT USED]]";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "YaneuraOu ⇔ rshogi SPSA パラメータマッピング表を生成"
)]
struct Cli {
    /// YaneuraOu 形式の .params (例: tune/suisho10.params)
    #[arg(long)]
    yo_params: PathBuf,

    /// rshogi 形式の .params (例: spsa_params/suisho10_converted.params)
    #[arg(long)]
    rshogi_params: PathBuf,

    /// 出力先 TOML
    #[arg(long)]
    output: PathBuf,
}

fn parse_value_i32(text: &str) -> Result<i32> {
    if let Ok(v) = text.parse::<i32>() {
        return Ok(v);
    }
    let v = text.parse::<f64>().with_context(|| format!("invalid numeric value: {text}"))?;
    Ok(v.round() as i32)
}

/// YaneuraOu と rshogi のいずれの `.params` も同じ CSV 形式。
fn load_params(path: &PathBuf) -> Result<BTreeMap<String, i32>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut map = BTreeMap::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.with_context(|| format!("line {line_no}: read failed"))?;
        let mut raw = line.trim().to_owned();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        if raw.contains(NOT_USED_MARKER) {
            raw = raw.replace(NOT_USED_MARKER, "");
        }
        if let Some((head, _)) = raw.split_once("//") {
            raw = head.to_owned();
        }
        let cols: Vec<&str> = raw.split(',').map(str::trim).collect();
        if cols.len() < 3 {
            bail!("line {line_no} in {}: not enough columns", path.display());
        }
        let name = cols[0].to_owned();
        let value = parse_value_i32(cols[2])
            .with_context(|| format!("line {line_no} in {}", path.display()))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn rshogi_defaults() -> HashMap<String, i32> {
    SearchTuneParams::option_specs()
        .iter()
        .map(|spec| (spec.usi_name.to_owned(), spec.default))
        .collect()
}

#[derive(Debug, Clone)]
struct AutoMatch {
    yo: String,
    rshogi: String,
    sign_flip: bool,
}

/// rshogi 名 + その値 + (候補YO名, 符号反転) のリスト
type AmbiguousEntry = (String, i32, Vec<(String, bool)>);

fn main() -> Result<()> {
    let cli = Cli::parse();
    let yo = load_params(&cli.yo_params)?;
    let rshogi = load_params(&cli.rshogi_params)?;
    let defaults = rshogi_defaults();

    // YO 値 → 名前リストの逆引き（同値が複数ある場合に備えて Vec）
    let mut yo_by_value: HashMap<i32, Vec<String>> = HashMap::new();
    for (name, val) in &yo {
        yo_by_value.entry(*val).or_default().push(name.clone());
    }

    let mut auto_matches: Vec<AutoMatch> = Vec::new();
    let mut ambiguous: Vec<AmbiguousEntry> = Vec::new();
    let mut rshogi_unmapped: Vec<String> = Vec::new();
    let mut yo_used: HashSet<String> = HashSet::new();

    for (rname, rval) in &rshogi {
        let default = defaults.get(rname).copied();
        let is_tuned = match default {
            Some(d) => *rval != d,
            None => true,
        };
        if !is_tuned {
            // rshogi default のままなら YO から転記された値ではない（YO 側に該当なし）
            continue;
        }
        let mut candidates: Vec<(String, bool)> = Vec::new();
        if let Some(names) = yo_by_value.get(rval) {
            for n in names {
                candidates.push((n.clone(), false));
            }
        }
        // P2-2: rval == 0 のときは -rval == rval なので二重カウントを避ける
        if *rval != 0
            && let Some(names) = yo_by_value.get(&-*rval)
        {
            for n in names {
                candidates.push((n.clone(), true));
            }
        }
        match candidates.len() {
            0 => rshogi_unmapped.push(rname.clone()),
            1 => {
                let (yname, flip) = candidates.into_iter().next().unwrap();
                // P2-1: 既に別の rshogi 名に割り当て済みの YO 名なら、一意性を壊すので
                // ambiguous に振り分けて人手判断させる
                if yo_used.contains(yname.as_str()) {
                    ambiguous.push((rname.clone(), *rval, vec![(yname, flip)]));
                } else {
                    yo_used.insert(yname.clone());
                    auto_matches.push(AutoMatch {
                        yo: yname,
                        rshogi: rname.clone(),
                        sign_flip: flip,
                    });
                }
            }
            _ => ambiguous.push((rname.clone(), *rval, candidates)),
        }
    }

    // YO 側で未使用のもの（rshogi に対応する rshogi param がない、または曖昧)
    let yo_unmapped: Vec<String> =
        yo.keys().filter(|n| !yo_used.contains(n.as_str())).cloned().collect();

    // 出力
    let mut out = File::create(&cli.output)
        .with_context(|| format!("failed to create {}", cli.output.display()))?;
    writeln!(out, "# YaneuraOu ⇔ rshogi SPSA パラメータマッピング表")?;
    writeln!(out, "#")?;
    writeln!(out, "# このファイルは build_param_mapping により自動生成されたあと、")?;
    writeln!(out, "# 人手でレビュー・追加修正することを想定しています。")?;
    writeln!(out, "#")?;
    writeln!(
        out,
        "# - sign_flip = true: YO 式の `-X *` を rshogi 側で値の符号に内包しているため"
    )?;
    writeln!(out, "#   YO 値 X に対し rshogi 値は -X となる。")?;
    writeln!(
        out,
        "# - ambiguous セクション: 値が他のパラメータと衝突しており人手判断が必要。"
    )?;
    writeln!(out, "# - unmapped_yo: rshogi 側に対応がない YO パラメータ。")?;
    writeln!(out, "# - unmapped_rshogi: YO 側から転記されていない rshogi パラメータ。")?;
    writeln!(out)?;

    auto_matches.sort_by(|a, b| a.rshogi.cmp(&b.rshogi));
    for m in &auto_matches {
        writeln!(out, "[[mapping]]")?;
        writeln!(out, "yo = \"{}\"", m.yo)?;
        writeln!(out, "rshogi = \"{}\"", m.rshogi)?;
        writeln!(out, "sign_flip = {}", m.sign_flip)?;
        writeln!(out)?;
    }

    if !ambiguous.is_empty() {
        writeln!(out, "[ambiguous]")?;
        writeln!(out, "# rshogi -> [候補YO名:符号反転]")?;
        ambiguous.sort_by(|a, b| a.0.cmp(&b.0));
        for (rname, rval, cands) in &ambiguous {
            let cand_strs: Vec<String> = cands
                .iter()
                .map(|(n, f)| format!("{}{}", if *f { "-" } else { "" }, n))
                .collect();
            writeln!(out, "# {} (rshogi value = {}): {}", rname, rval, cand_strs.join(", "))?;
        }
        writeln!(out)?;
    }

    if !yo_unmapped.is_empty() {
        writeln!(out, "[unmapped]")?;
        let mut s = yo_unmapped.clone();
        s.sort();
        writeln!(out, "yo = [")?;
        for n in &s {
            writeln!(out, "  \"{n}\",")?;
        }
        writeln!(out, "]")?;
    } else {
        writeln!(out, "[unmapped]")?;
        writeln!(out, "yo = []")?;
    }

    let mut s = rshogi_unmapped.clone();
    s.sort();
    writeln!(out, "rshogi = [")?;
    for n in &s {
        writeln!(out, "  \"{n}\",")?;
    }
    writeln!(out, "]")?;

    eprintln!(
        "auto-matched = {}, ambiguous = {}, yo_unmapped = {}, rshogi_unmapped = {}",
        auto_matches.len(),
        ambiguous.len(),
        yo_unmapped.len(),
        rshogi_unmapped.len(),
    );
    eprintln!("wrote {}", cli.output.display());

    Ok(())
}
