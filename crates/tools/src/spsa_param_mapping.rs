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

pub const NOT_USED_MARKER: &str = "[[NOT USED]]";

/// `.params` ファイルの 1 行を文字列のまま保持する低レベル表現。
///
/// 値の型変換 (i32 / f64) は呼び出し側 (`ParamRow::from_raw` /
/// `SpsaParam::from_raw`) が担当する。`SpsaParam` は値が浮動小数で `comment` を
/// 保持する一方、`ParamRow` は整数化して `comment` を捨てる、と用途が異なるため、
/// 共通する「コメント分離 + `[[NOT USED]]` 検出 + カラム分割」だけをここで一本化する。
#[derive(Debug, Clone)]
pub struct RawParamRow {
    pub name: String,
    pub kind: String,
    pub value_text: String,
    pub min_text: String,
    pub max_text: String,
    /// 6 番目の列。YO/rshogi `.params` では `step`、Fishtest 派生では `c_end`。
    pub col5_text: String,
    /// 7 番目の列。YO/rshogi `.params` では `alpha`、Fishtest 派生では `r_end`。
    pub col6_text: String,
    /// `//` 以降の文字列（先頭の `//` を含まずトリム済み、空なら "")
    pub comment: String,
    pub not_used: bool,
}

/// `.params` の 1 行を `RawParamRow` にパースする。
///
/// 戻り値:
/// - `Ok(None)`: 空行 / `#` コメント行（無視）
/// - `Ok(Some(row))`: 通常行
/// - `Err(_)`: カラム数不足
///
/// 注意: コメント (`// 以降`) を先に切り離してから `[[NOT USED]]` 判定を行う。
/// 順序を逆にすると `// 旧: [[NOT USED]]` のようなコメント内のマーカーまで誤検出する。
pub fn parse_param_line(line: &str, line_no: usize) -> Result<Option<RawParamRow>> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }
    let (raw_val_part, comment) = if let Some((left, right)) = trimmed.split_once("//") {
        (left, right.trim().to_string())
    } else {
        (trimmed, String::new())
    };
    let not_used = raw_val_part.contains(NOT_USED_MARKER);
    let val_owned: String;
    let val_part: &str = if not_used {
        val_owned = raw_val_part.replace(NOT_USED_MARKER, "");
        val_owned.as_str()
    } else {
        raw_val_part
    };
    let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
    if cols.len() < 7 {
        bail!("line {line_no}: expected 7 columns, got {} ('{line}')", cols.len());
    }
    Ok(Some(RawParamRow {
        name: cols[0].to_owned(),
        kind: cols[1].to_owned(),
        value_text: cols[2].to_owned(),
        min_text: cols[3].to_owned(),
        max_text: cols[4].to_owned(),
        col5_text: cols[5].to_owned(),
        col6_text: cols[6].to_owned(),
        comment,
        not_used,
    }))
}

/// `.params` ファイルを行ごとに `RawParamRow` の列として読み込む共通ヘルパ。
///
/// 空行と `#` コメント行は無視される。
pub fn read_raw_params(path: &Path) -> Result<Vec<RawParamRow>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.with_context(|| format!("line {line_no}: read failed"))?;
        if let Some(row) =
            parse_param_line(&line, line_no).with_context(|| format!("in {}", path.display()))?
        {
            rows.push(row);
        }
    }
    Ok(rows)
}

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

/// `.params` の value 列を i32 に正規化する共通ヘルパ。整数値ならそのまま、
/// 小数値なら `round()` する（YO/rshogi いずれの `.params` も整数前提）。
pub fn parse_value_i32(text: &str) -> Result<i32> {
    if let Ok(v) = text.parse::<i32>() {
        return Ok(v);
    }
    let v = text.parse::<f64>().with_context(|| format!("invalid numeric value: {text}"))?;
    Ok(v.round() as i32)
}

fn parse_f64(text: &str) -> Result<f64> {
    text.parse::<f64>().with_context(|| format!("invalid float value: {text}"))
}

impl ParamRow {
    /// `RawParamRow` から整数値ベースの `ParamRow` を構築する（コメントは捨てる）。
    pub fn from_raw(raw: RawParamRow) -> Result<Self> {
        Ok(Self {
            value: parse_value_i32(&raw.value_text)
                .with_context(|| format!("invalid value '{}'", raw.value_text))?,
            min: parse_value_i32(&raw.min_text)
                .with_context(|| format!("invalid min '{}'", raw.min_text))?,
            max: parse_value_i32(&raw.max_text)
                .with_context(|| format!("invalid max '{}'", raw.max_text))?,
            step: parse_f64(&raw.col5_text)
                .with_context(|| format!("invalid step '{}'", raw.col5_text))?,
            alpha: parse_f64(&raw.col6_text)
                .with_context(|| format!("invalid alpha '{}'", raw.col6_text))?,
            name: raw.name,
            kind: raw.kind,
            not_used: raw.not_used,
        })
    }
}

/// `.params` を順序保存で読み込む
pub fn load_params(path: &Path) -> Result<Vec<ParamRow>> {
    let raws = read_raw_params(path)?;
    let mut rows = Vec::with_capacity(raws.len());
    for raw in raws {
        rows.push(
            ParamRow::from_raw(raw)
                .with_context(|| format!("failed to parse row in {}", path.display()))?,
        );
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

    /// YO 名 / rshogi 名の双方向ルックアップを 1 度だけ構築するインデックスを返す。
    ///
    /// 旧 `by_yo_name` / `by_rshogi_name` は呼ぶたびに `HashMap` を新規 alloc していた。
    /// ループ内での誤用を避けるため、1 度作って取り回す API に統一した。
    pub fn index(&self) -> MappingIndex<'_> {
        MappingIndex {
            by_yo: self.mappings.iter().map(|m| (m.yo.as_str(), m)).collect(),
            by_rshogi: self.mappings.iter().map(|m| (m.rshogi.as_str(), m)).collect(),
        }
    }
}

/// `MappingTable::index()` で構築する双方向ルックアップ。
///
/// `&Mapping` は `MappingTable` への借用なので、構築元の `MappingTable` より
/// 長生きしてはならない（lifetime `'a` が制約する）。
#[derive(Debug)]
pub struct MappingIndex<'a> {
    by_yo: HashMap<&'a str, &'a Mapping>,
    by_rshogi: HashMap<&'a str, &'a Mapping>,
}

impl<'a> MappingIndex<'a> {
    /// YO 名から `Mapping` を引く
    pub fn by_yo(&self, name: &str) -> Option<&'a Mapping> {
        self.by_yo.get(name).copied()
    }

    /// rshogi 名から `Mapping` を引く
    pub fn by_rshogi(&self, name: &str) -> Option<&'a Mapping> {
        self.by_rshogi.get(name).copied()
    }

    /// YO 名がインデックスに登録されているか
    pub fn contains_yo(&self, name: &str) -> bool {
        self.by_yo.contains_key(name)
    }

    /// rshogi 名がインデックスに登録されているか
    pub fn contains_rshogi(&self, name: &str) -> bool {
        self.by_rshogi.contains_key(name)
    }
}

/// 値変換: YO → rshogi
///
/// 内部実装は `rshogi_to_yo_value` と同一（involution）だが、両関数を別名で公開する
/// ことで呼び出し元の方向性（どちらの名前空間にいるのか）を明示する目的で残している。
pub fn yo_to_rshogi_value(yo_value: i32, sign_flip: bool) -> i32 {
    if sign_flip { -yo_value } else { yo_value }
}

/// 値変換: rshogi → YO（YO 側は元の符号慣用に戻る）
///
/// `yo_to_rshogi_value` と実装は同じ。意図表現（どちら方向の変換か）を保つために両方公開。
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
    fn parse_param_line_basic() {
        let raw = parse_param_line("foo,int,10,0,100,1.0,0.002", 1).unwrap().unwrap();
        assert_eq!(raw.name, "foo");
        assert_eq!(raw.kind, "int");
        assert_eq!(raw.value_text, "10");
        assert_eq!(raw.min_text, "0");
        assert_eq!(raw.max_text, "100");
        assert_eq!(raw.col5_text, "1.0");
        assert_eq!(raw.col6_text, "0.002");
        assert!(!raw.not_used);
        assert!(raw.comment.is_empty());
    }

    #[test]
    fn parse_param_line_skips_blank_and_comment() {
        assert!(parse_param_line("", 1).unwrap().is_none());
        assert!(parse_param_line("   ", 2).unwrap().is_none());
        assert!(parse_param_line("# whole-line comment", 3).unwrap().is_none());
    }

    #[test]
    fn parse_param_line_extracts_inline_comment_and_not_used() {
        let raw = parse_param_line("foo,int,10,0,100,1,0.002 [[NOT USED]] // 旧: tuned away", 5)
            .unwrap()
            .unwrap();
        assert!(raw.not_used);
        assert_eq!(raw.comment, "旧: tuned away");
        assert_eq!(raw.value_text, "10");
    }

    /// `// 旧: [[NOT USED]]` のようにコメント内にマーカーがあっても
    /// not_used を偽陽性で立てない（先にコメント分離する規約の回帰テスト）
    #[test]
    fn parse_param_line_marker_in_comment_only_is_ignored() {
        let raw = parse_param_line("foo,int,10,0,100,1,0.002 // 旧: [[NOT USED]]", 7)
            .unwrap()
            .unwrap();
        assert!(!raw.not_used);
        assert_eq!(raw.comment, "旧: [[NOT USED]]");
    }

    #[test]
    fn parse_param_line_rejects_short_row() {
        let err = parse_param_line("foo,int,10,0,100", 9).unwrap_err();
        assert!(err.to_string().contains("expected 7 columns"));
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
    ///
    /// fixture 不在時は `panic!` で明示的に失敗する（CI で `--ignored` を回した時に
    /// fixture 配置漏れがサイレントに通過しないようにするため）。
    #[test]
    #[ignore]
    fn canonical_pair_round_trip() {
        let yo_path = Path::new("tune/suisho10.params");
        let rshogi_path = Path::new("tune/suisho10_converted.params");
        let mapping_path = Path::new("tune/yo_rshogi_mapping.toml");
        for p in &[yo_path, rshogi_path, mapping_path] {
            assert!(
                p.exists(),
                "fixture not present: {} — see test doc for placement",
                p.display()
            );
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
