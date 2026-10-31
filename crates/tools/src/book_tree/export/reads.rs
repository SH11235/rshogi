use super::numbers;
use crate::book_db::{position_from_sfen, strip_ply};
use anyhow::{Context, Result, ensure};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::Signed;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Reading {
    pub nodes: BigRational,
    pub ply: BigInt,
    pub scores: Vec<(String, BigRational)>,
}
pub(super) type Groups = BTreeMap<String, Vec<Reading>>;
#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Counts {
    pub lines: usize,
    pub malformed: usize,
    pub below_min: usize,
    pub below_min_positions: usize,
    pub unsorted: usize,
    pub illegal_top: usize,
    pub draw_pinned: usize,
    pub duplicates: usize,
    pub used: usize,
    pub draw_unset_used: usize,
}
pub(super) fn expand(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    for path in paths {
        ensure!(path.exists(), "log input does not exist: {}", path.display());
        if path.is_dir() {
            for item in walkdir::WalkDir::new(path) {
                let item = item.with_context(|| format!("読めません: {}", path.display()))?;
                if item.path().extension().is_some_and(|s| s == "jsonl") {
                    files.insert(
                        item.path()
                            .canonicalize()
                            .with_context(|| format!("読めません: {}", item.path().display()))?,
                    );
                }
            }
        } else {
            files.insert(
                path.canonicalize().with_context(|| format!("読めません: {}", path.display()))?,
            );
        }
    }
    ensure!(!files.is_empty(), "no JSONL inputs found");
    Ok(files.into_iter().collect())
}
fn canonical_key(key: &str) -> Result<String> {
    let parts: Vec<_> = key.split_whitespace().collect();
    ensure!(
        parts.len() == 3
            && parts[0].matches('K').count() == 1
            && parts[0].matches('k').count() == 1,
        "invalid SFEN key"
    );
    Ok(strip_ply(&position_from_sfen(&format!("{} 1", parts.join(" ")))?.to_sfen()).into())
}
type RawObject = BTreeMap<String, Box<RawValue>>;

// 先に型付きフィールドへ読むと、重複キーの最後の値を選ぶ前に型エラーになる。
#[derive(Deserialize)]
#[serde(try_from = "RawObject")]
struct ReadEvent {
    key: String,
    nodes: Box<RawValue>,
    ply: Box<RawValue>,
    multipv: Vec<RawObject>,
    draw: Option<RawObject>,
}

fn take_raw(object: &mut RawObject, field: &str) -> Result<Box<RawValue>> {
    object.remove(field).with_context(|| format!("missing {field}"))
}

fn take_field<T: serde::de::DeserializeOwned>(object: &mut RawObject, field: &str) -> Result<T> {
    Ok(serde_json::from_str(take_raw(object, field)?.get())?)
}

impl TryFrom<RawObject> for ReadEvent {
    type Error = anyhow::Error;

    fn try_from(mut fields: RawObject) -> Result<Self> {
        Ok(Self {
            key: take_field(&mut fields, "key")?,
            nodes: take_raw(&mut fields, "nodes")?,
            ply: take_raw(&mut fields, "ply")?,
            multipv: take_field(&mut fields, "multipv")?,
            draw: fields
                .remove("draw")
                .map(|v| serde_json::from_str(v.get()))
                .transpose()?
                .flatten(),
        })
    }
}

fn parse(line: &str) -> Result<(String, Reading, bool)> {
    let event: ReadEvent = serde_json::from_str(line)?;
    let draw = event.draw.is_some_and(|o| !o.is_empty());
    let key = canonical_key(&event.key)?;
    let nodes = numbers::number(&event.nodes)?;
    let ply = event.ply.get();
    ensure!(!ply.contains(['.', 'e', 'E']), "invalid ply");
    let ply: BigInt = ply.parse()?;
    ensure!(ply >= BigInt::from(1), "invalid ply");
    ensure!(!event.multipv.is_empty(), "empty multipv");
    let mut seen = BTreeSet::new();
    let mut scores = Vec::new();
    for mut entry in event.multipv {
        let mv: String = take_field(&mut entry, "move")?;
        ensure!(seen.insert(mv.clone()), "duplicate move");
        scores.push((mv, numbers::number(&take_raw(&mut entry, "cp")?)?));
    }
    Ok((key, Reading { nodes, ply, scores }, draw))
}
type Fingerprint = [u8; 16];

fn key_fingerprint(key: &str) -> Fingerprint {
    let digest = Sha256::digest(key.as_bytes());
    let mut result = [0; 16];
    result.copy_from_slice(&digest[..16]);
    result
}

fn reading_fingerprint(key: &str, reading: &Reading) -> Fingerprint {
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        // 可変長の値の連結で、フィールド境界が異なる読みを同一視しないため。
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    fn rational(hash: &mut Sha256, value: &BigRational) {
        field(hash, &value.numer().to_signed_bytes_le());
        field(hash, &value.denom().to_signed_bytes_le());
    }
    let mut hash = Sha256::new();
    field(&mut hash, key.as_bytes());
    rational(&mut hash, &reading.nodes);
    field(&mut hash, &reading.ply.to_signed_bytes_le());
    for (mv, cp) in &reading.scores {
        field(&mut hash, mv.as_bytes());
        rational(&mut hash, cp);
    }
    let digest = hash.finalize();
    let mut result = [0; 16];
    result.copy_from_slice(&digest[..16]);
    result
}

fn remember<K: Ord>(seen: &mut BTreeMap<K, bool>, key: K, draw: bool, counts: &mut Counts) {
    match seen.entry(key) {
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            counts.duplicates += 1;
            if draw && !*entry.get() {
                entry.insert(true);
                counts.draw_unset_used -= 1;
            }
        }
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(draw);
            counts.used += 1;
            if !draw {
                counts.draw_unset_used += 1;
            }
        }
    }
}

/// 読みを集計し、`retain_keys` が `None` なら全局面、`Some` なら指定局面の読みを返す。
pub(super) fn load(
    paths: &[PathBuf],
    min_nodes: impl Into<BigInt>,
    draw_pin: impl Into<BigInt>,
    retain_keys: Option<&BTreeSet<&str>>,
) -> Result<(Groups, Counts)> {
    let minimum = BigRational::from_integer(min_nodes.into());
    let pin = BigRational::from_integer(draw_pin.into());
    ensure!(!minimum.is_negative() && !pin.is_negative(), "thresholds must be nonnegative");
    let mut seen: BTreeMap<String, BTreeMap<Reading, bool>> = BTreeMap::new();
    let mut summaries = BTreeMap::new();
    let mut below = BTreeSet::new();
    let mut counts = Counts::default();
    for path in paths {
        load_file(path, &mut |line| {
            counts.lines += 1;
            let Ok((key, reading, draw)) = parse(line) else {
                counts.malformed += 1;
                return Ok(());
            };
            if reading.nodes < minimum {
                counts.below_min += 1;
                below.insert(key_fingerprint(&key));
                return Ok(());
            }
            if reading.scores.windows(2).any(|s| s[0].1 < s[1].1) {
                counts.unsorted += 1;
                return Ok(());
            }
            let pos = position_from_sfen(&format!("{key} 1"))?;
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            if !legal.iter().any(|m| m.to_usi() == reading.scores[0].0) {
                counts.illegal_top += 1;
                return Ok(());
            }
            if !draw && reading.scores[0].1.abs() <= pin {
                counts.draw_pinned += 1;
                return Ok(());
            }
            if retain_keys.is_none_or(|keys| keys.contains(key.as_str())) {
                remember(seen.entry(key).or_default(), reading, draw, &mut counts);
            } else {
                remember(&mut summaries, reading_fingerprint(&key, &reading), draw, &mut counts);
            }
            Ok(())
        })?;
    }
    counts.below_min_positions = below.len();
    let groups = seen
        .into_iter()
        .map(|(key, readings)| (key, readings.into_keys().collect()))
        .collect();
    Ok((groups, counts))
}
fn load_file(path: &Path, process: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
    for (i, line) in BufReader::new(
        std::fs::File::open(path).with_context(|| format!("開けません: {}", path.display()))?,
    )
    .lines()
    .enumerate()
    {
        let line = line.with_context(|| format!("読めません: {}", path.display()))?;
        let line = if i == 0 {
            line.strip_prefix('\u{feff}').unwrap_or(&line)
        } else {
            &line
        };
        if !line.trim().is_empty() {
            process(line)?;
        }
    }
    Ok(())
}
pub(super) fn deep(reads: &[Reading]) -> Vec<&Reading> {
    let Some(max) = reads.iter().map(|r| &r.nodes).max() else {
        return vec![];
    };
    let mut kept: Vec<_> = reads.iter().filter(|r| &r.nodes * BigInt::from(3) > *max).collect();
    kept.sort();
    kept
}

#[cfg(test)]
mod tests {
    use super::super::super::reply_graph::HIRATE_KEY;
    use super::*;
    fn event(nodes: &str, ply: &str, cp: &str, mv: &str, draw: &str) -> String {
        format!(
            r#"{{"key":"{HIRATE_KEY}","nodes":{nodes},"ply":{ply},"multipv":[{{"move":"{mv}","cp":{cp}}}],"draw":{draw}}}"#
        )
    }
    #[test]
    fn exclusions_duplicates_draw_and_depth_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reads.jsonl");
        let good = event("900000000", "1", "12", "7g7f", "null");
        let lines = vec![
            "not json".into(),
            event("true", "1", "12", "7g7f", "null"),
            event("900000000", "true", "12", "7g7f", "null"),
            event("900000000", "1.0", "12", "7g7f", "null"),
            event("900000000", "0", "12", "7g7f", "null"),
            event("900000000", "1", "true", "7g7f", "null"),
            event("900000000", "1", "12", "7g7f", "true"),
            event("899999999.9", "1", "12", "7g7f", "null"),
            event("900000000", "1", "12", "9i9a", "null"),
            event("900000000", "1", "3", "7g7f", "{}"),
            good.clone(),
            good,
            event("900000000", "1", "12", "7g7f", r#"{"b":0}"#),
            event("2700000000", "1", "20", "2g2f", "null"),
            event("900000000.1", "1", "13", "7g7f", "null"),
        ];
        std::fs::write(&path, format!("\u{feff}{}\n\n", lines.join("\n"))).unwrap();
        let (groups, c) = load(std::slice::from_ref(&path), 900_000_000, 3, None).unwrap();
        for keys in [BTreeSet::new(), BTreeSet::from([HIRATE_KEY])] {
            let (selected, counts) =
                load(std::slice::from_ref(&path), 900_000_000, 3, Some(&keys)).unwrap();
            assert_eq!(counts, c);
            if keys.is_empty() {
                assert!(selected.is_empty());
            } else {
                assert_eq!(selected, groups);
            }
        }
        assert_eq!(
            (
                c.lines,
                c.malformed,
                c.below_min,
                c.below_min_positions,
                c.illegal_top,
                c.draw_pinned,
                c.used,
                c.duplicates,
                c.draw_unset_used
            ),
            (15, 7, 1, 1, 1, 1, 3, 2, 2)
        );
        let kept = deep(&groups[HIRATE_KEY]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].nodes, numbers::decimal("900000000.1").unwrap());
    }
    #[test]
    fn multipv_order_duplicate_and_illegal_lower_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reads.jsonl");
        let prefix = format!(r#"{{"key":"{HIRATE_KEY}","nodes":100,"ply":1,"multipv":"#);
        let lines = [
            r#"[]}"#,
            r#"[{"move":"7g7f","cp":10},{"move":"7g7f","cp":9}]}"#,
            r#"[{"move":"7g7f","cp":10},{"move":"2g2f","cp":11}]}"#,
            r#"[{"move":"7g7f","cp":10},{"move":"bad","cp":9}]}"#,
        ];
        std::fs::write(&path, lines.map(|l| format!("{prefix}{l}\n")).concat()).unwrap();
        let (groups, c) = load(&[path], 0, 0, None).unwrap();
        assert_eq!((c.malformed, c.unsorted, c.used), (2, 1, 1));
        assert_eq!(groups[HIRATE_KEY][0].scores[1].0, "bad");
    }
    #[test]
    fn raw_fields_preserve_numbers_and_last_duplicate_values() {
        let line = format!(
            r#"{{"key":null,"key":"{HIRATE_KEY}","nodes":false,"nodes":9007199254740993,
            "ply":[],"ply":184467440737095516160,"draw":false,"draw":{{"b":1e400}},
            "multipv":null,"multipv":[{{"move":false,"move":"7g7f","cp":null,
            "cp":1.00000000000000011102230246251565404236316680908203126}}]}}"#
        );
        let (key, reading, draw) = parse(&line).unwrap();
        assert_eq!(key, HIRATE_KEY);
        assert!(draw);
        assert_eq!(reading.nodes, numbers::decimal("9007199254740993").unwrap());
        assert_eq!(reading.ply.to_string(), "184467440737095516160");
        assert_eq!(reading.scores[0].1, numbers::decimal("1.0000000000000002").unwrap());
        let good = event("100", "1", "12", "7g7f", "null");
        for (field, value) in [
            ("key", "null"),
            ("nodes", "false"),
            ("ply", "1.0"),
            ("draw", "[]"),
            ("multipv", "{}"),
        ] {
            let line = format!(r#"{},"{field}":{value}}}"#, &good[..good.len() - 1]);
            assert!(parse(&line).is_err(), "{line}");
        }
        let line = good.replace(r#""cp":12"#, r#""cp":12,"cp":false"#);
        assert!(parse(&line).is_err());
    }

    #[test]
    fn missing_wrong_types_and_nonfinite_fields_are_malformed() {
        let good = event("100", "1", "12", "7g7f", "null");
        let mut lines = Vec::new();
        for field in ["key", "nodes", "ply", "multipv"] {
            let mut object: RawObject = serde_json::from_str(&good).unwrap();
            object.remove(field);
            lines.push(serde_json::to_string(&object).unwrap());
        }
        for value in [
            r#""12""#,
            "true",
            "false",
            "null",
            "[]",
            "{}",
            "NaN",
            "Infinity",
            "-Infinity",
            "1e400",
        ] {
            lines.push(event(value, "1", "12", "7g7f", "null"));
            lines.push(event("100", value, "12", "7g7f", "null"));
            lines.push(event("100", "1", value, "7g7f", "null"));
        }
        for value in [r#""draw""#, "true", "false", "[]", "1"] {
            lines.push(event("100", "1", "12", "7g7f", value));
        }
        for value in ["null", "true", "[]", r#""event""#] {
            lines.push(value.into());
        }
        for value in [
            "null",
            "{}",
            "[null]",
            "[{}]",
            r#"[{"move":"7g7f"}]"#,
            r#"[{"cp":12}]"#,
            r#"[{"move":12,"cp":12}]"#,
        ] {
            let mut object: RawObject = serde_json::from_str(&good).unwrap();
            object.insert("multipv".into(), serde_json::from_str(value).unwrap());
            lines.push(serde_json::to_string(&object).unwrap());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reads.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let (groups, counts) = load(&[path], 0, 0, None).unwrap();
        assert!(groups.is_empty());
        assert_eq!(counts.lines, lines.len());
        assert_eq!(counts.malformed, lines.len());
        for draw in ["null", "{}"] {
            assert!(!parse(&event("100", "1", "12", "7g7f", draw)).unwrap().2);
        }
        assert!(!parse(&good.replace(r#","draw":null"#, "")).unwrap().2);
    }

    #[test]
    fn fingerprints_preserve_normalized_identity_and_score_order() {
        let (key, reading, _) = parse(&event("100", "1", "12", "7g7f", "null")).unwrap();
        let expected = reading_fingerprint(&key, &reading);
        let (_, equivalent, _) = parse(&event("1e2", "1", "12.0", "7g7f", r#"{"b":0}"#)).unwrap();
        assert_eq!(reading_fingerprint(&key, &equivalent), expected);
        for line in [
            event("101", "1", "12", "7g7f", "null"),
            event("100", "2", "12", "7g7f", "null"),
            event("100", "1", "-12", "7g7f", "null"),
            event("100", "1", "12.5", "7g7f", "null"),
            event("100", "1", "12", "2g2f", "null"),
        ] {
            let (_, changed, _) = parse(&line).unwrap();
            assert_ne!(reading_fingerprint(&key, &changed), expected);
        }
        assert_ne!(reading_fingerprint("other key", &reading), expected);
        let mut ordered = reading;
        ordered.scores.push(("2g2f".into(), BigRational::from_integer(12.into())));
        let forward = reading_fingerprint(&key, &ordered);
        assert_ne!(forward, expected);
        ordered.scores.reverse();
        assert_ne!(reading_fingerprint(&key, &ordered), forward);
    }

    #[test]
    fn recursive_sorted_paths_and_missing_inputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("sub/b.jsonl");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        assert_eq!(expand(&[dir.path().into(), a.clone()]).unwrap(), [a, b]);
        assert!(expand(&[dir.path().join("missing")]).is_err());
        assert!(expand(&[]).is_err());
    }
}
