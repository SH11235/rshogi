//! 元データの語彙と読み条件の明示的な表。
use super::master_table::{self, Row};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

const CONDITIONS: &str = "name\tfingerprint\tstop\tnodes_counting\tthreads\thash_mb\tmultipv\tdraw_black\tdraw_white\tengine_sha256\teval\tmachine\tnote";

pub(super) fn paths(dir: &Path) -> [PathBuf; 3] {
    ["sources.tsv", "conditions.tsv", "lineage_kinds.tsv"].map(|name| dir.join(name))
}

pub(super) fn valid_fingerprint(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Default)]
pub(super) struct Schema {
    pub(super) sources: Vec<(String, String)>,
    conditions: Vec<(String, String)>,
    lineage_kinds: Vec<String>,
    replaceable: BTreeSet<String>,
    explicit: bool,
    prefixes: Mutex<BTreeMap<String, String>>,
}

fn table(path: &Path, header: &str) -> Result<Vec<Vec<String>>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("読めません: {}", path.display()))?;
    let mut lines = text.lines();
    ensure!(lines.next() == Some(header), "{}: 見出しが違います", path.display());
    let width = header.split('\t').count();
    let mut rows = Vec::new();
    for (i, line) in lines.enumerate() {
        let fields: Vec<_> = line.split('\t').map(str::to_owned).collect();
        ensure!(
            fields.len() == width && !line.contains('\r'),
            "{}:{}: 不正な行",
            path.display(),
            i + 2
        );
        rows.push(fields);
    }
    Ok(rows)
}

impl Schema {
    pub(super) fn load(dir: Option<&Path>) -> Result<Self> {
        let Some(dir) = dir else {
            return Ok(Self::default());
        };
        ensure!(dir.is_dir(), "schema is not a directory: {}", dir.display());
        let [sources, conditions, kinds] = paths(dir);
        let mut schema = Self {
            explicit: true,
            ..Self::default()
        };
        let mut seen = BTreeSet::new();
        for row in table(&sources, "source\tdescription\treplaceable")? {
            ensure!(
                !row[0].trim().is_empty()
                    && !row[1].trim().is_empty()
                    && matches!(row[2].as_str(), "0" | "1"),
                "sources.tsv: 不正な行"
            );
            ensure!(seen.insert(row[0].clone()), "sources.tsv: duplicate source");
            if row[2] == "1" {
                schema.replaceable.insert(row[0].clone());
            }
            schema.sources.push((row[0].clone(), row[1].clone()));
        }
        seen.clear();
        let mut fingerprints = BTreeSet::new();
        for row in table(&conditions, CONDITIONS)? {
            ensure!(
                master_table::condition_name(&row[0])
                    && !matches!(row[0].as_str(), "unknown" | "mixed"),
                "conditions.tsv: 不正な name"
            );
            ensure!(seen.insert(row[0].clone()), "conditions.tsv: duplicate name");
            let fp = row[1].to_ascii_lowercase();
            if !fp.is_empty() {
                ensure!(
                    valid_fingerprint(&fp),
                    "conditions.tsv: fingerprint は 64 桁の 16 進にしてください"
                );
                ensure!(fingerprints.insert(fp.clone()), "conditions.tsv: duplicate fingerprint");
            }
            schema.conditions.push((row[0].clone(), fp));
        }
        seen.clear();
        for row in table(&kinds, "kind\tdescription")? {
            ensure!(master_table::lineage_kind(&row[0]), "lineage_kinds.tsv: 不正な kind");
            ensure!(seen.insert(row[0].clone()), "lineage_kinds.tsv: duplicate kind");
            schema.lineage_kinds.push(row[0].clone());
        }
        Ok(schema)
    }

    pub(super) fn replaceable(&self, source: &str) -> bool {
        if self.explicit {
            self.replaceable.contains(source)
        } else {
            ["read", "deep", "ext"].contains(&source)
        }
    }

    pub(super) fn condition(&self, fingerprint: &str) -> Result<String> {
        ensure!(valid_fingerprint(fingerprint), "fingerprint は 64 桁の 16 進にしてください");
        let fingerprint = fingerprint.to_ascii_lowercase();
        if !self.explicit {
            let prefix = fingerprint[..8].to_owned();
            let mut prefixes = self
                .prefixes
                .lock()
                .map_err(|_| anyhow::anyhow!("条件名の照合に失敗しました"))?;
            let previous = prefixes.entry(prefix.clone()).or_insert_with(|| fingerprint.clone());
            ensure!(
                previous == &fingerprint,
                "指紋の先頭 8 文字 {prefix} が衝突しています。--schema の conditions.tsv で別の名前を付けてください"
            );
            return Ok(prefix);
        }
        self.conditions
            .iter()
            .find(|(_, fp)| fp == &fingerprint)
            .map(|(name, _)| name.clone())
            .with_context(|| {
                format!("conditions.tsv に fingerprint {fingerprint} の行を追加してください")
            })
    }

    fn vocabulary(&self, row: &Row) -> Result<()> {
        if !self.explicit {
            return Ok(());
        }
        ensure!(
            self.sources.iter().any(|(s, _)| s == row.get("source")),
            "unknown source: {}",
            row.get("source")
        );
        let cond = row.get("cond");
        ensure!(
            matches!(cond, "" | "unknown" | "mixed")
                || self.conditions.iter().any(|(name, _)| name == cond),
            "unknown cond: {cond}"
        );
        for entry in row.get("lineage").split(',').filter(|s| !s.is_empty()) {
            let kind = entry.split_once(':').context("不正な lineage")?.0;
            ensure!(self.lineage_kinds.iter().any(|k| k == kind), "unknown lineage kind: {kind}");
        }
        Ok(())
    }

    pub(super) fn validate_row(&self, row: &Row) -> Result<()> {
        master_table::validate_metadata(row)?;
        self.vocabulary(row)
    }

    pub(super) fn vocabulary_errors(&self, text: impl AsRef<[u8]>) -> Result<Vec<(usize, String)>> {
        let mut errors = Vec::new();
        if self.explicit {
            for (i, line) in text.as_ref().split(|b| *b == b'\n').enumerate().skip(1) {
                if let Ok(line) = std::str::from_utf8(line)
                    && let Ok(row) = Row::parse_line(line.trim_end_matches('\r'), i + 1)
                    && let Err(error) = self.vocabulary(&row)
                {
                    errors.push((i + 1, format!("元データの {} 行目: {error}", i + 1)));
                }
            }
        }
        Ok(errors)
    }

    pub(super) fn validate(&self, text: impl AsRef<[u8]>) -> Result<Vec<String>> {
        let mut errors = master_table::validate_table(text.as_ref())?.errors;
        errors.extend(self.vocabulary_errors(text)?);
        errors.sort_by_key(|(at, _)| *at);
        Ok(errors.into_iter().map(|(_, message)| message).collect())
    }
}

#[cfg(test)]
pub(super) fn test_schema(dir: &Path) {
    std::fs::write(dir.join("conditions.tsv"), format!("{CONDITIONS}\n")).unwrap();
    std::fs::write(
        dir.join("lineage_kinds.tsv"),
        "kind\tdescription\nbook\t外部の定跡\nplan\t計画\n",
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn files(dir: &Path) {
        test_schema(dir);
        std::fs::write(
            dir.join("sources.tsv"),
            "source\tdescription\treplaceable\ncustom\t独自の読み\t1\nread\t保護\t0\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("conditions.tsv"),
            format!(
                "{CONDITIONS}\n{}\nlegacy\t\t\t\t\t\t\t\t\t\t\t\t\n",
                ["named".to_owned(), "a".repeat(64)]
                    .into_iter()
                    .chain(std::iter::repeat_n(String::new(), 11))
                    .collect::<Vec<_>>()
                    .join("\t")
            ),
        )
        .unwrap();
    }
    #[test]
    fn abbreviated_fingerprints_must_be_unambiguous() {
        let schema = Schema::default();
        let first = "a".repeat(64);
        let second = format!("aaaaaaaa{}", "b".repeat(56));
        assert_eq!(schema.condition(&first).unwrap(), "aaaaaaaa");
        assert_eq!(schema.condition(&first.to_uppercase()).unwrap(), "aaaaaaaa");
        let error = schema.condition(&second).unwrap_err().to_string();
        assert!(error.contains("--schema") && error.contains("conditions.tsv"));
        let dir = tempfile::tempdir().unwrap();
        files(dir.path());
        let path = dir.path().join("conditions.tsv");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(
            &["second".to_owned(), second.clone()]
                .into_iter()
                .chain(std::iter::repeat_n(String::new(), 11))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        text.push('\n');
        std::fs::write(path, text).unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        assert_eq!(schema.condition(&first).unwrap(), "named");
        assert_eq!(schema.condition(&second).unwrap(), "second");
    }

    #[test]
    fn missing_invalid_and_duplicate_tables_never_fall_back() {
        let d = tempfile::tempdir().unwrap();
        assert!(Schema::load(Some(&d.path().join("missing"))).is_err());
        for name in ["sources.tsv", "conditions.tsv", "lineage_kinds.tsv"] {
            files(d.path());
            let path = d.path().join(name);
            std::fs::remove_file(&path).unwrap();
            assert!(Schema::load(Some(d.path())).is_err(), "{name}");
            std::fs::write(&path, "wrong\n").unwrap();
            assert!(Schema::load(Some(d.path())).is_err(), "{name}");
            files(d.path());
            let mut text = std::fs::read_to_string(&path).unwrap();
            let duplicate = text.lines().nth(1).unwrap().to_owned();
            text.push_str(&format!("{duplicate}\n"));
            std::fs::write(path, text).unwrap();
            assert!(Schema::load(Some(d.path())).is_err(), "{name}");
        }
        for (name, from, to) in [
            ("sources.tsv", "\t1\n", "\t2\n"),
            ("conditions.tsv", "named", "bad name"),
            ("conditions.tsv", "named", "unknown"),
            ("conditions.tsv", "aaaaaaaa", "xxxxxxxy"),
            ("lineage_kinds.tsv", "book", "Book"),
        ] {
            files(d.path());
            let path = d.path().join(name);
            let text = std::fs::read_to_string(&path).unwrap().replace(from, to);
            std::fs::write(path, text).unwrap();
            assert!(Schema::load(Some(d.path())).is_err(), "{name}: {to}");
        }
        files(d.path());
        let path = d.path().join("conditions.tsv");
        let text = std::fs::read_to_string(&path).unwrap();
        let duplicate = text.lines().nth(1).unwrap().replace("named", "second").replace('a', "A");
        std::fs::write(path, format!("{text}{duplicate}\n")).unwrap();
        assert!(Schema::load(Some(d.path())).is_err());
    }
    #[test]
    fn vocabularies_conditions_and_replaceable_sources() {
        let d = tempfile::tempdir().unwrap();
        files(d.path());
        let schema = Schema::load(Some(d.path())).unwrap();
        assert!(schema.replaceable("custom"));
        assert!(!schema.replaceable("read"));
        assert!(!schema.replaceable("absent"));
        assert!(Schema::default().replaceable("read"));
        assert!(!Schema::default().replaceable("custom"));
        assert_eq!(schema.condition(&"A".repeat(64)).unwrap(), "named");
        assert!(schema.condition(&"b".repeat(64)).unwrap_err().to_string().contains("追加"));
        assert_eq!(Schema::default().condition(&"b".repeat(64)).unwrap(), "bbbbbbbb");
        assert!(schema.condition("short").is_err());
        let good = master_table::test_row(&[
            ("source", "custom".into()),
            ("evals", "1".into()),
            ("cond", "named".into()),
            ("value_kind", "single".into()),
            ("lineage", "book:x,plan:y".into()),
        ]);
        assert!(schema.validate_row(&good).is_ok());
        for (column, value) in [
            ("source", "missing"),
            ("cond", "missing"),
            ("lineage", "missing:x"),
        ] {
            let mut row = good.clone();
            row.set(column, value.into());
            assert!(schema.validate_row(&row).is_err());
            assert!(Schema::default().validate_row(&row).is_ok());
        }
        for cond in ["unknown", "mixed", "legacy"] {
            let mut row = good.clone();
            row.set("cond", cond.into());
            assert!(schema.validate_row(&row).is_ok());
        }
    }
}
