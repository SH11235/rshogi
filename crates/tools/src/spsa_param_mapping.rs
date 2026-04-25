//! YaneuraOu ⇔ rshogi SPSA `.params` 変換のための共有モジュール
//!
//! - `.params` (CSV) の読み書き
//! - `tune/yo_rshogi_mapping.toml` のロード
//! - YO ⇔ rshogi 名前変換ヘルパ

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const NOT_USED_MARKER: &str = "[[NOT USED]]";

/// `.params` ファイルの 1 行（YO/rshogi 共通フォーマット）
#[derive(Debug, Clone)]
pub struct ParamRow {
    pub name: String,
    pub kind: String,
    pub value: i32,
    pub min: i32,
    pub max: i32,
    pub step: f64,
    pub alpha: f64,
    pub not_used: bool,
}

fn parse_value_i32(text: &str) -> Result<i32> {
    if let Ok(v) = text.parse::<i32>() {
        return Ok(v);
    }
    let v = text.parse::<f64>().with_context(|| format!("invalid numeric value: {text}"))?;
    Ok(v.round() as i32)
}

fn parse_f64(text: &str) -> Result<f64> {
    text.parse::<f64>().with_context(|| format!("invalid float value: {text}"))
}

/// `.params` を順序保存で読み込む
pub fn load_params(path: &Path) -> Result<Vec<ParamRow>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.with_context(|| format!("line {line_no}: read failed"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 先にコメント (`//` 以降) を切り離し、値部分にだけ `[[NOT USED]]` 判定を
        // 適用する。順序を逆にするとコメント内のマーカーまで消えて偽陽性になる。
        let val_part = match trimmed.split_once("//") {
            Some((left, _)) => left.trim(),
            None => trimmed,
        };
        let not_used = val_part.contains(NOT_USED_MARKER);
        let raw = if not_used {
            val_part.replace(NOT_USED_MARKER, "")
        } else {
            val_part.to_owned()
        };
        let cols: Vec<&str> = raw.split(',').map(str::trim).collect();
        if cols.len() < 7 {
            bail!("line {line_no} in {}: expected 7 columns, got {}", path.display(), cols.len());
        }
        let row = ParamRow {
            name: cols[0].to_owned(),
            kind: cols[1].to_owned(),
            value: parse_value_i32(cols[2])
                .with_context(|| format!("line {line_no} in {}: value", path.display()))?,
            min: parse_value_i32(cols[3])
                .with_context(|| format!("line {line_no} in {}: min", path.display()))?,
            max: parse_value_i32(cols[4])
                .with_context(|| format!("line {line_no} in {}: max", path.display()))?,
            step: parse_f64(cols[5])
                .with_context(|| format!("line {line_no} in {}: step", path.display()))?,
            alpha: parse_f64(cols[6])
                .with_context(|| format!("line {line_no} in {}: alpha", path.display()))?,
            not_used,
        };
        rows.push(row);
    }
    Ok(rows)
}

/// `.params` を書き出す（YO/rshogi 共通フォーマット）
pub fn write_params(path: &Path, rows: &[ParamRow]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    for r in rows {
        write!(
            w,
            "{},{},{},{},{},{},{}",
            r.name, r.kind, r.value, r.min, r.max, r.step, r.alpha
        )?;
        if r.not_used {
            write!(w, " {NOT_USED_MARKER}")?;
        }
        writeln!(w)?;
    }
    w.flush()?;
    Ok(())
}

/// マッピング表 1 エントリ
#[derive(Deserialize, Debug, Clone)]
pub struct Mapping {
    pub yo: String,
    pub rshogi: String,
    pub sign_flip: bool,
}

#[derive(Deserialize, Debug, Default)]
pub struct UnmappedSection {
    #[serde(default)]
    pub yo: Vec<String>,
    #[serde(default)]
    pub rshogi: Vec<String>,
}

/// `yo_rshogi_mapping.toml` の構造
#[derive(Deserialize, Debug)]
pub struct MappingTable {
    #[serde(default, rename = "mapping")]
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub unmapped: UnmappedSection,
}

impl MappingTable {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read mapping table {}", path.display()))?;
        let table: MappingTable = toml::from_str(&text)
            .with_context(|| format!("failed to parse mapping table {}", path.display()))?;
        table.validate()?;
        Ok(table)
    }

    /// 名前一意性等の整合性チェック
    pub fn validate(&self) -> Result<()> {
        let mut yo_seen: HashMap<&str, usize> = HashMap::new();
        let mut rshogi_seen: HashMap<&str, usize> = HashMap::new();
        for (i, m) in self.mappings.iter().enumerate() {
            if let Some(prev) = yo_seen.insert(m.yo.as_str(), i) {
                bail!("mapping entry #{i}: YO name '{}' is duplicated (also at #{prev})", m.yo);
            }
            if let Some(prev) = rshogi_seen.insert(m.rshogi.as_str(), i) {
                bail!(
                    "mapping entry #{i}: rshogi name '{}' is duplicated (also at #{prev})",
                    m.rshogi
                );
            }
        }
        for n in &self.unmapped.rshogi {
            if rshogi_seen.contains_key(n.as_str()) {
                bail!("unmapped.rshogi includes '{n}' which is also in mappings");
            }
        }
        for n in &self.unmapped.yo {
            if yo_seen.contains_key(n.as_str()) {
                bail!("unmapped.yo includes '{n}' which is also in mappings");
            }
        }
        Ok(())
    }

    pub fn by_yo_name(&self) -> HashMap<&str, &Mapping> {
        self.mappings.iter().map(|m| (m.yo.as_str(), m)).collect()
    }

    pub fn by_rshogi_name(&self) -> HashMap<&str, &Mapping> {
        self.mappings.iter().map(|m| (m.rshogi.as_str(), m)).collect()
    }
}

/// 値変換: YO → rshogi
pub fn yo_to_rshogi_value(yo_value: i32, sign_flip: bool) -> i32 {
    if sign_flip { -yo_value } else { yo_value }
}

/// 値変換: rshogi → YO（YO 側は元の符号慣用に戻る）
pub fn rshogi_to_yo_value(rshogi_value: i32, sign_flip: bool) -> i32 {
    if sign_flip {
        -rshogi_value
    } else {
        rshogi_value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// YO ↔ rshogi の値変換は involution（2 回適用で元に戻る）
    #[test]
    fn value_translation_is_involution() {
        for &v in &[-10000_i32, -1, 0, 1, 100, 12345] {
            for &flip in &[false, true] {
                let r = yo_to_rshogi_value(v, flip);
                assert_eq!(rshogi_to_yo_value(r, flip), v, "v={v} flip={flip}");
            }
        }
    }

    fn make_table(entries: &[(&str, &str, bool)]) -> MappingTable {
        MappingTable {
            mappings: entries
                .iter()
                .map(|(yo, rs, fl)| Mapping {
                    yo: (*yo).to_owned(),
                    rshogi: (*rs).to_owned(),
                    sign_flip: *fl,
                })
                .collect(),
            unmapped: UnmappedSection::default(),
        }
    }

    #[test]
    fn validate_detects_duplicate_yo() {
        let t = make_table(&[("a", "X", false), ("a", "Y", false)]);
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_detects_duplicate_rshogi() {
        let t = make_table(&[("a", "X", false), ("b", "X", true)]);
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_detects_unmapped_overlap() {
        let mut t = make_table(&[("a", "X", false)]);
        t.unmapped.rshogi.push("X".to_owned());
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_detects_unmapped_yo_overlap() {
        let mut t = make_table(&[("a", "X", false)]);
        t.unmapped.yo.push("a".to_owned());
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_accepts_unique_table() {
        let t = make_table(&[("a", "X", false), ("b", "Y", true), ("c", "Z", false)]);
        assert!(t.validate().is_ok());
    }

    /// 正本 .params ペア (suisho10.params + suisho10_converted.params) で
    /// YO → rshogi → YO ラウンドトリップが値を保つことを確認する回帰テスト。
    ///
    /// テストデータが環境依存なので `#[ignore]` 付き。
    /// 実行には以下を `tune/` 配下に配置してから `cargo test -p tools -- --ignored`:
    /// - `tune/suisho10.params`
    /// - `tune/suisho10_converted.params` (= spsa_params/suisho10_converted.params のコピー)
    /// - `tune/yo_rshogi_mapping.toml`
    #[test]
    #[ignore]
    fn canonical_pair_round_trip() {
        let yo_path = Path::new("tune/suisho10.params");
        let rshogi_path = Path::new("tune/suisho10_converted.params");
        let mapping_path = Path::new("tune/yo_rshogi_mapping.toml");
        if !yo_path.exists() || !rshogi_path.exists() || !mapping_path.exists() {
            eprintln!("fixture files not present, skipping");
            return;
        }
        let table = MappingTable::load(mapping_path).expect("mapping load");
        let yo = load_params(yo_path).expect("yo load");
        let r = load_params(rshogi_path).expect("rshogi load");

        let yo_by_name: HashMap<&str, &ParamRow> =
            yo.iter().map(|x| (x.name.as_str(), x)).collect();
        let r_by_name: HashMap<&str, &ParamRow> = r.iter().map(|x| (x.name.as_str(), x)).collect();

        let mut checked = 0;
        for m in &table.mappings {
            let (Some(yo_row), Some(r_row)) =
                (yo_by_name.get(m.yo.as_str()), r_by_name.get(m.rshogi.as_str()))
            else {
                continue;
            };
            let to_r = yo_to_rshogi_value(yo_row.value, m.sign_flip);
            assert_eq!(
                to_r, r_row.value,
                "{} -> {}: YO={} sign_flip={} 期待 rshogi={} 実際={}",
                m.yo, m.rshogi, yo_row.value, m.sign_flip, to_r, r_row.value
            );
            let back_to_yo = rshogi_to_yo_value(r_row.value, m.sign_flip);
            assert_eq!(
                back_to_yo, yo_row.value,
                "{} -> {}: rshogi={} sign_flip={} 期待 YO={} 実際={}",
                m.yo, m.rshogi, r_row.value, m.sign_flip, back_to_yo, yo_row.value
            );
            checked += 1;
        }
        assert!(checked >= 90, "checked too few mappings: {checked}");
    }
}
