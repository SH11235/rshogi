use super::{Schema, master_table::Row};
use crate::book_db::{position_from_sfen, strip_ply};
use crate::book_tree::journal;
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Default)]
pub(super) struct Depths {
    fingerprints: BTreeSet<String>,
    readings: BTreeMap<(String, String), journal::Reading>,
}

impl Depths {
    pub(super) fn depths(&self, schema: &Schema, row: &Row) -> Result<Option<Vec<u32>>> {
        let cond = row.get("cond");
        let fingerprint = if let Some(fp) = schema.fingerprint_for(cond) {
            fp
        } else if !schema.is_explicit()
            && cond.len() == 8
            && cond.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let prefix = cond.to_ascii_lowercase();
            let mut matches = self.fingerprints.iter().filter(|fp| fp.starts_with(&prefix));
            let Some(fp) = matches.next() else {
                return Ok(None);
            };
            ensure!(matches.next().is_none(), "条件の指紋の先頭が衝突しています: {cond}");
            fp
        } else {
            return Ok(None);
        };
        let mut depths: Vec<_> = self
            .readings
            .get(&(fingerprint.to_owned(), row.get("key").to_owned()))
            .into_iter()
            .filter_map(|reading| reading.multipv.iter().find(|pv| pv.mv == row.get("move")))
            .map(|pv| pv.depth)
            .collect();
        depths.sort_unstable();
        Ok(Some(depths))
    }
}

pub(super) fn load(paths: &[PathBuf], keys: &BTreeSet<&str>) -> Result<Depths> {
    let mut result = Depths::default();
    let mut engine = 0;
    let mut other = 0;
    let mut duplicates = 0;
    let mut used = 0;
    let mut invalid = 0;
    let mut conflicting = 0;
    let mut tails = 0;
    let mut skipped = 0;
    let scan = (|| -> Result<()> {
        for path in paths {
            let scanned = journal::visit_records(path, |rec| {
                if rec.provenance != "engine" {
                    other += 1;
                    return Ok(());
                }
                engine += 1;
                let validated = (|| -> Result<_> {
                    ensure!(
                        rec.settings.computed_fingerprint()? == rec.settings.fingerprint,
                        "台帳の指紋と条件が一致しません"
                    );
                    let key = strip_ply(
                        &position_from_sfen(&format!("{} 1", strip_ply(&rec.reading.key)))?
                            .to_sfen(),
                    )
                    .to_owned();
                    let mut reading = rec.reading;
                    reading.key = key.clone();
                    reading.validate(&rec.settings)?;
                    Ok((key, reading))
                })();
                let Ok((key, reading)) = validated else {
                    invalid += 1;
                    return Ok(());
                };
                let fp = rec.settings.fingerprint;
                result.fingerprints.insert(fp.clone());
                if !keys.contains(key.as_str()) {
                    return Ok(());
                }
                match result.readings.entry((fp, key)) {
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        if entry.get().same_values(&reading) {
                            duplicates += 1;
                        } else {
                            conflicting += 1;
                        }
                    }
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(reading);
                        used += 1;
                    }
                }
                Ok(())
            })?;
            if let Some(ignored_tail) = scanned {
                tails += usize::from(ignored_tail);
            } else {
                skipped += 1;
                eprintln!("journal: skipped (not a tree journal): {}", path.display());
            }
        }
        Ok(())
    })();
    eprintln!(
        "journal: engine={engine}, non-engine={other}, used={used}, duplicates={duplicates}, invalid={invalid}, conflicting={conflicting}, ignored-tail={tails}, skipped-files={skipped}, errors={}",
        usize::from(scan.is_err())
    );
    scan?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_collision_requires_schema_and_empty_schema_fingerprint_has_no_depths() {
        let depths = Depths {
            fingerprints: BTreeSet::from(["a".repeat(64), format!("aaaaaaaa{}", "b".repeat(56))]),
            ..Depths::default()
        };
        let row = crate::book_tree::master_table::test_row(&[("cond", "aaaaaaaa".into())]);
        let error = depths.depths(&Schema::default(), &row).unwrap_err();
        assert!(error.to_string().contains("衝突"));

        let dir = tempfile::tempdir().unwrap();
        crate::book_tree::schema::test_schema(dir.path());
        std::fs::write(dir.path().join("sources.tsv"), "source\tdescription\treplaceable\n")
            .unwrap();
        let path = dir.path().join("conditions.tsv");
        let mut table = std::fs::read_to_string(&path).unwrap();
        table.push_str(&format!("aaaaaaaa{}\n", "\t".repeat(12)));
        std::fs::write(path, table).unwrap();
        let schema = Schema::load(Some(dir.path())).unwrap();
        assert_eq!(depths.depths(&schema, &row).unwrap(), None);
    }
}
