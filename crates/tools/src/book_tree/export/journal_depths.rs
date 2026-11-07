use super::{Schema, master_table::Row};
use crate::book_db::{position_from_sfen, strip_ply};
use crate::book_tree::{journal, schema::valid_fingerprint};
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Default)]
pub(super) struct Depths {
    fingerprints: BTreeSet<String>,
    readings: BTreeMap<(String, String), Vec<journal::Reading>>,
}

impl Depths {
    pub(super) fn depths(&self, schema: &Schema, row: &Row) -> Result<Option<Vec<u32>>> {
        let cond = row.get("cond");
        let fingerprint = if let Some(fp) = schema.fingerprint_for(cond) {
            fp
        } else if cond.len() == 8 && cond.bytes().all(|b| b.is_ascii_hexdigit()) {
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
            .flatten()
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
                ensure!(valid_fingerprint(&rec.settings.fingerprint), "台帳の指紋が不正です");
                let fp = rec.settings.fingerprint.to_ascii_lowercase();
                result.fingerprints.insert(fp.clone());
                let key = strip_ply(
                    &position_from_sfen(&format!("{} 1", strip_ply(&rec.reading.key)))?.to_sfen(),
                )
                .to_owned();
                if !keys.contains(key.as_str()) {
                    return Ok(());
                }
                let mut reading = rec.reading;
                reading.key = key.clone();
                reading.validate(&rec.settings)?;
                let readings = result.readings.entry((fp, key)).or_default();
                if readings.iter().any(|old| old.same_values(&reading)) {
                    duplicates += 1;
                } else {
                    readings.push(reading);
                    used += 1;
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
    if !paths.is_empty() {
        eprintln!(
            "journal: engine={engine}, non-engine={other}, used={used}, duplicates={duplicates}, ignored-tail={tails}, skipped-files={skipped}, errors={}",
            usize::from(scan.is_err())
        );
    }
    scan?;
    Ok(result)
}
