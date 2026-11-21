use super::{Schema, master_table::Row};
use crate::book_db::{position_from_sfen, strip_ply};
use crate::book_tree::journal;
use anyhow::{Result, ensure};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
};

#[derive(Default)]
pub(super) struct Depths {
    fingerprints: BTreeSet<String>,
    readings: BTreeMap<(String, String), (journal::Reading, bool)>,
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
            .filter_map(|(reading, _)| reading.multipv.iter().find(|pv| pv.mv == row.get("move")))
            .map(|pv| pv.depth)
            .collect();
        depths.sort_unstable();
        Ok(Some(depths))
    }
}

struct ValidatedReading {
    fingerprint: String,
    reading: journal::Reading,
    engine: bool,
}

fn validate_record(rec: journal::Record) -> Result<ValidatedReading> {
    ensure!(
        rec.settings.computed_fingerprint()? == rec.settings.fingerprint,
        "台帳の指紋と条件が一致しません"
    );
    let mut reading = rec.reading;
    reading.key =
        strip_ply(&position_from_sfen(&format!("{} 1", strip_ply(&reading.key)))?.to_sfen())
            .to_owned();
    reading.validate(&rec.settings)?;
    Ok(ValidatedReading {
        fingerprint: rec.settings.fingerprint,
        reading,
        engine: rec.provenance == "engine",
    })
}

enum Batch {
    Records {
        records: Vec<ValidatedReading>,
        engine: usize,
        other: usize,
        invalid: usize,
    },
    Finished(Result<Option<bool>>),
}

fn send_records(
    records: Vec<journal::Record>,
    engine: usize,
    other: usize,
    sender: &SyncSender<Batch>,
    pool: &rayon::ThreadPool,
) -> Result<()> {
    let candidates = records.len();
    // 後続ファイルが送信待ちでも、先頭ファイルの検証を複数スレッドで進める。
    let records: Vec<_> = pool.install(|| {
        records
            .into_par_iter()
            .with_min_len(8)
            .filter_map(|rec| validate_record(rec).ok())
            .collect()
    });
    let invalid = candidates - records.len();
    sender
        .send(Batch::Records {
            records,
            engine,
            other,
            invalid,
        })
        .map_err(|_| anyhow::anyhow!("台帳の読み込みを中断しました"))
}

fn read_batches(
    path: &Path,
    sender: &SyncSender<Batch>,
    cancelled: &AtomicBool,
    pool: &rayon::ThreadPool,
) {
    let mut records = Vec::new();
    let mut engine = 0;
    let mut other = 0;
    let scanned = journal::visit_records(path, |record| {
        anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "台帳の読み込みを中断しました");
        let is_engine = record.provenance == "engine";
        if is_engine || record.provenance.starts_with("journal:") {
            engine += usize::from(is_engine);
            records.push(record);
        } else {
            other += 1;
        }
        if records.len() == 64 {
            send_records(
                std::mem::take(&mut records),
                std::mem::take(&mut engine),
                std::mem::take(&mut other),
                sender,
                pool,
            )?;
        }
        Ok(())
    });
    if (!records.is_empty() || other > 0)
        && send_records(records, engine, other, sender, pool).is_err()
    {
        return;
    }
    let _ = sender.send(Batch::Finished(scanned));
}

fn visit_files(
    paths: &[PathBuf],
    pool: &rayon::ThreadPool,
    mut visit: impl FnMut(Vec<ValidatedReading>, usize, usize, usize),
    mut finished: impl FnMut(&Path, Option<bool>),
) -> Result<()> {
    let workers = pool.current_num_threads().min(paths.len());
    let cancelled = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| -> Result<()> {
        let (announcements, assignments) = sync_channel(workers);
        let mut receivers = Vec::new();
        let mut readers = Vec::new();
        // 先行ファイルの数によらず、検証済みの束はワーカーごとの送信枠だけに保持する。
        for worker in 0..workers {
            let (sender, receiver) = sync_channel(2);
            receivers.push(receiver);
            let announcements = announcements.clone();
            let cancelled = &cancelled;
            let next = &next;
            readers.push(scope.spawn(move || {
                while !cancelled.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(index) else {
                        break;
                    };
                    if announcements.send((index, worker)).is_err() {
                        break;
                    }
                    read_batches(path, &sender, cancelled, pool);
                }
            }));
        }
        drop(announcements);
        let stopped = || anyhow::anyhow!("台帳の読み込みワーカーが停止しました");
        let result = (|| {
            let mut assigned = BTreeMap::new();
            for (index, path) in paths.iter().enumerate() {
                while !assigned.contains_key(&index) {
                    let (file, worker) = assignments.recv().map_err(|_| stopped())?;
                    assigned.insert(file, worker);
                }
                let worker = assigned.remove(&index).ok_or_else(stopped)?;
                loop {
                    match receivers[worker].recv().map_err(|_| stopped())? {
                        Batch::Records {
                            records,
                            engine,
                            other,
                            invalid,
                        } => {
                            visit(records, engine, other, invalid);
                        }
                        Batch::Finished(scanned) => {
                            finished(path, scanned?);
                            break;
                        }
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            cancelled.store(true, Ordering::Relaxed);
        }
        drop(receivers);
        drop(assignments);
        for reader in readers {
            reader.join().map_err(|_| stopped())?;
        }
        result
    })
}

pub(super) fn load(
    paths: &[PathBuf],
    keys: &BTreeSet<&str>,
    pool: &rayon::ThreadPool,
) -> Result<Depths> {
    let mut result = Depths::default();
    let mut engine = 0;
    let mut imported = 0;
    let mut other = 0;
    let mut duplicates = 0;
    let mut used = 0;
    let mut invalid = 0;
    let mut conflicting = 0;
    let mut tails = 0;
    let mut skipped = 0;
    let mut visit_record = |rec: ValidatedReading| {
        let fp = rec.fingerprint;
        let reading = rec.reading;
        let key = reading.key.clone();
        result.fingerprints.insert(fp.clone());
        if !keys.contains(key.as_str()) {
            return;
        }
        match result.readings.entry((fp, key)) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (previous, previous_is_engine) = entry.get();
                if previous.same_values(&reading) {
                    duplicates += 1;
                } else {
                    conflicting += 1;
                }
                // 合流した台帳を先に渡しても、元の台帳がある局面ではその読みの深さを保つ。
                if rec.engine && !previous_is_engine {
                    entry.insert((reading, true));
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((reading, rec.engine));
                used += 1;
            }
        }
    };
    let scan = visit_files(
        paths,
        pool,
        |records, engine_rows, other_rows, invalid_rows| {
            engine += engine_rows;
            imported += records.len() + invalid_rows - engine_rows;
            other += other_rows;
            invalid += invalid_rows;
            for record in records {
                visit_record(record);
            }
        },
        |path, scanned| {
            if let Some(ignored_tail) = scanned {
                tails += usize::from(ignored_tail);
            } else {
                skipped += 1;
                eprintln!("journal: skipped (not a tree journal): {}", path.display());
            }
        },
    );
    eprintln!(
        "journal: engine={engine}, imported={imported}, other={other}, used={used}, duplicates={duplicates}, invalid={invalid}, conflicting={conflicting}, ignored-tail={tails}, skipped-files={skipped}, errors={}",
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
