use super::journal::{
    ConflictSink, Journal, OnConflict, Reading, Settings, StagedSource, parse_record,
};
use super::transfer::ConflictTsv;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CHUNK: usize = 1024;

#[derive(Default, Serialize)]
struct Counts {
    appended: usize,
    skipped_same: usize,
    conflicts: usize,
    skipped_condition: usize,
    skipped_invalid: usize,
    skipped_conflicting: usize,
}

struct Staged {
    reader: BufReader<File>,
    sha256: String,
    row_marks: BufReader<File>,
    list: ConflictTsv,
    counts: Counts,
    eof: bool,
    ignored_tail: bool,
}

// 一覧はファイル全体を終えてから公開し、各周回の台帳追記より先に一時ファイルを同期する。
struct CheckpointList<'a>(&'a mut ConflictTsv);
impl ConflictSink for CheckpointList<'_> {
    fn conflict(&mut self, old: &Reading, new: &Reading, provenance: &str) -> Result<()> {
        self.0.conflict(old, new, provenance)
    }
    fn before_append(&mut self) -> Result<()> {
        self.0.checkpoint()
    }
}

/// serve が lock を保持したまま、検査済みの読みを少量ずつ台帳へ取り込む受付。
pub(super) struct Inbox {
    dir: PathBuf,
    path: Option<PathBuf>,
    preparing: Option<JoinHandle<Result<StagedSource>>>,
    staged: Option<Staged>,
    status: serde_json::Value,
    discarded_worker: usize,
    appended: BTreeSet<String>,
}

impl Inbox {
    /// 受付と処理済み・失敗ファイルの置き場を作る。
    pub fn new(state: &Path) -> Result<Self> {
        let dir = state.join("import");
        for path in [&dir, &dir.join("done"), &dir.join("failed")] {
            super::paths::reject_symlink(path)?;
            std::fs::create_dir_all(path)?;
        }
        Ok(Self {
            dir,
            path: None,
            preparing: None,
            staged: None,
            status: serde_json::json!({}),
            discarded_worker: 0,
            appended: BTreeSet::new(),
        })
    }

    /// 最新の取り込みの件数と失敗理由。worker の後着結果を捨てた件数も含む。
    pub fn status(&self) -> serde_json::Value {
        let mut status = self.status.clone();
        status["discarded_worker"] = self.discarded_worker.into();
        status
    }

    /// 直前の周回で追記した局面。
    pub fn appended(&self) -> &BTreeSet<String> {
        &self.appended
    }

    /// 先に台帳へ入った読みを残し、worker の後着結果を数える。
    pub fn complete_worker(
        &mut self,
        journal: &mut Journal,
        settings: &Settings,
        reading: Reading,
        prefetch: bool,
        mut alert: impl FnMut(&str),
    ) -> Result<bool> {
        if let Some(old) = journal.get_exact(&reading.key, &settings.fingerprint) {
            if !old.same_values(&reading) {
                alert(&format!("worker の後着結果が台帳と食い違います: {}", reading.key));
            }
            self.discarded_worker += 1;
            println!("import discarded_worker={}", self.discarded_worker);
            return Ok(false);
        }
        journal.append_marked(reading, settings, "engine".into(), prefetch)?;
        Ok(true)
    }

    /// 一周回分を進める。入力の失敗は通知して次へ進み、台帳の書き込み失敗は呼び元に返す。
    pub fn poll(&mut self, journal: &mut Journal) -> Result<Option<String>> {
        self.poll_chunk(journal, CHUNK)
    }

    #[cfg(test)]
    pub fn poll_short(&mut self, journal: &mut Journal) -> Result<Option<String>> {
        self.poll_chunk(journal, 64)
    }

    fn poll_chunk(&mut self, journal: &mut Journal, limit: usize) -> Result<Option<String>> {
        self.appended.clear();
        if self.path.is_none() {
            let mut paths = std::fs::read_dir(&self.dir)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
            paths.sort();
            if let Some(path) = paths.into_iter().next() {
                let source = path.clone();
                self.status = serde_json::json!({"file":path.file_name(), "state":"validating"});
                self.path = Some(path);
                self.preparing =
                    Some(thread::spawn(move || super::journal::stage_live_source(&source)));
            }
            return Ok(None);
        }
        let path = self.path.as_ref().context("取り込み元がありません")?;
        if self.preparing.as_ref().is_some_and(|h| h.is_finished()) {
            let prepared = self
                .preparing
                .take()
                .context("検査がありません")?
                .join()
                .map_err(|_| anyhow::anyhow!("取り込み元の検査が panic"))
                .and_then(|r| r);
            match prepared {
                Ok(source) => {
                    let list = ConflictTsv::create(&self.dir.join("done").join(format!(
                        "{}.conflicts.tsv",
                        path.file_name().context("名前がありません")?.to_string_lossy()
                    )))?;
                    self.staged = Some(Staged {
                        reader: BufReader::new(source.file),
                        sha256: source.sha256,
                        row_marks: BufReader::new(source.row_marks),
                        list,
                        counts: Counts {
                            skipped_condition: source.skipped_condition,
                            skipped_invalid: source.skipped_invalid,
                            ..Counts::default()
                        },
                        eof: false,
                        ignored_tail: source.ignored_tail,
                    });
                }
                Err(error) => {
                    let message = format!("台帳の取り込み失敗 {}: {error:#}", path.display());
                    self.status = serde_json::json!({"file":path.file_name(), "state":"failed", "reason":message});
                    std::fs::rename(
                        path,
                        self.dir.join("failed").join(path.file_name().context("名前がありません")?),
                    )?;
                    crate::common::io::sync_directory(&self.dir.join("failed"))?;
                    crate::common::io::sync_directory(&self.dir)?;
                    self.path = None;
                    return Ok(Some(message));
                }
            }
        }
        let Some(staged) = self.staged.as_mut() else {
            return Ok(None);
        };
        let start = Instant::now();
        let mut incoming = Vec::new();
        let mut prefetched = BTreeSet::new();
        for index in 0..limit {
            if staged.eof || (index > 0 && start.elapsed() >= Duration::from_millis(5)) {
                break;
            }
            let mut bytes = Vec::new();
            if staged.reader.read_until(b'\n', &mut bytes)? == 0 {
                staged.eof = true;
                break;
            }
            let rec = parse_record(&bytes)?;
            let mut marks = [0];
            staged.row_marks.read_exact(&mut marks)?;
            if marks[0] & 4 != 0 {
                staged.counts.skipped_conflicting += 1;
                continue;
            }
            let existing = journal.get_exact(&rec.reading.key, &rec.settings.fingerprint);
            // 食い違いは重複行も一覧に残す。未追記の重複は、この周回の最初の行で取り込む。
            if (existing.is_some_and(|old| old.same_values(&rec.reading)) && marks[0] & 1 == 0)
                || (marks[0] & 2 != 0 && existing.is_none())
            {
                staged.counts.skipped_same += 1;
                continue;
            }
            let provenance = format!(
                "journal:sha256={}:fingerprint={}:force=false:path={}",
                staged.sha256,
                rec.settings.fingerprint,
                path.display()
            );
            if marks[0] & 1 != 0 {
                prefetched.insert((rec.settings.fingerprint.clone(), rec.reading.key.clone()));
            }
            if existing.is_none() {
                self.appended.insert(rec.reading.key.clone());
            }
            incoming.push((rec.reading, Arc::new(rec.settings), provenance));
        }
        if !incoming.is_empty() {
            let (appended, skipped, conflicts) = journal.merge_records(
                incoming,
                OnConflict::KeepExisting,
                &mut CheckpointList(&mut staged.list),
                &prefetched,
            )?;
            staged.counts.appended += appended;
            staged.counts.skipped_same += skipped - conflicts;
            staged.counts.conflicts += conflicts;
        }
        self.status = serde_json::json!({"file":path.file_name(), "state":"appending", "counts":staged.counts, "ignored_tail":staged.ignored_tail});
        if staged.eof {
            staged.list.before_append()?;
            std::fs::rename(
                path,
                self.dir.join("done").join(path.file_name().context("名前がありません")?),
            )?;
            crate::common::io::sync_directory(&self.dir.join("done"))?;
            crate::common::io::sync_directory(&self.dir)?;
            self.status["state"] = "done".into();
            println!("import {}", self.status);
            self.path = None;
            self.staged = None;
        }
        Ok(None)
    }
}
