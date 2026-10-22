use super::ReadingArgs;
use crate::book_db::{child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const VERSION: &str = "book-tree-v1/newgame-ready/node-calib-last-multipv-v1";

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update(digest(&entry.path())?.as_bytes());
        }
    } else {
        let mut file =
            File::open(path).with_context(|| format!("内容の指紋: {}", path.display()))?;
        let mut buf = [0; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Settings {
    pub fingerprint: String,
    pub version: String,
    pub engine_sha256: String,
    pub options: BTreeMap<String, String>,
    pub content_sha256: BTreeMap<String, String>,
    pub nodes: u64,
    pub multipv: usize,
    pub hash: u32,
    /// エンジンのソースの版 (`--source-version` の応答)。別マシンの読みを取り込むときの判定にだけ使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
}

impl Settings {
    pub fn new(args: &ReadingArgs) -> Result<Self> {
        if args.nodes == 0 || args.multipv == 0 || args.hash == 0 {
            bail!("nodes / multipv / hash は正数が必要です");
        }
        let mut options = BTreeMap::new();
        let mut names = BTreeSet::new();
        for arg in &args.engine_options {
            let (k, v) = arg.split_once('=').context("engine-option は K=V 形式です")?;
            let k = k.trim();
            if k.is_empty()
                || k.contains(['\n', '\r'])
                || v.contains(['\n', '\r'])
                || !names.insert(k.to_ascii_lowercase())
            {
                bail!("重複または不正な option: {k}");
            }
            options.insert(k.to_owned(), v.trim().to_owned());
        }
        for (k, v) in [
            ("Threads", "1".to_owned()),
            ("USI_Hash", args.hash.to_string()),
            ("MultiPV", args.multipv.to_string()),
        ] {
            if let Some((name, value)) =
                options.iter().find(|(name, _)| name.eq_ignore_ascii_case(k))
                && (name != k || value != &v)
            {
                bail!("{k} は {v} に固定してください");
            }
            options.insert(k.to_owned(), v);
        }
        if !options.keys().any(|k| matches!(k.as_str(), "EvalFile" | "EvalDir")) {
            bail!("既定モデルを推測できません。EvalFile / EvalDir を明示してください");
        }
        let mut content_sha256 = BTreeMap::new();
        for (k, v) in &options {
            let lower = k.to_ascii_lowercase();
            if ["file", "dir", "path", "coeff"].iter().any(|s| lower.ends_with(s)) {
                content_sha256.insert(k.clone(), digest(Path::new(v))?);
            }
        }
        if !options.contains_key("SPSAParamsFile") {
            let engine = std::fs::canonicalize(&args.engine)?;
            let auto = engine.parent().context("engine の親 dir がありません")?.join("spsa.params");
            let hash = if auto.try_exists()? {
                digest(&auto)?
            } else {
                "absent".into()
            };
            content_sha256.insert("implicit:SPSAParamsFile".into(), hash);
        }
        let mut settings = Self {
            fingerprint: String::new(),
            version: VERSION.into(),
            engine_sha256: digest(&args.engine)?,
            options,
            content_sha256,
            nodes: args.nodes,
            multipv: args.multipv,
            hash: args.hash,
            engine_version: None,
        };
        settings.fingerprint = settings.computed_fingerprint()?;
        Ok(settings)
    }

    /// 条件の指紋。ファイル型 option はパスでなく内容の SHA-256 で入れる。
    pub fn computed_fingerprint(&self) -> Result<String> {
        let mut normalized = self.clone();
        normalized.fingerprint = String::new();
        // 版は指紋に入れない (入れると版を記録する前の台帳と指紋が変わる)。
        normalized.engine_version = None;
        for (k, hash) in &self.content_sha256 {
            if normalized.options.contains_key(k) {
                normalized.options.insert(k.clone(), format!("sha256:{hash}"));
            }
        }
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&normalized)?)))
    }

    /// 版を除いて同じ条件か。
    pub fn same_apart_from_version(&self, other: &Self) -> bool {
        let strip = |s: &Self| Self {
            engine_version: None,
            ..s.clone()
        };
        strip(self) == strip(other)
    }

    pub fn draw(&self) -> BTreeMap<String, String> {
        self.options
            .iter()
            .filter(|(k, _)| matches!(k.as_str(), "DrawValueBlack" | "DrawValueWhite"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Pv {
    #[serde(rename = "move")]
    pub mv: String,
    pub cp: i32,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct Reading {
    pub tag: String,
    pub ply: u32,
    pub key: String,
    pub nodes_limit: u64,
    pub nodes: Option<u64>,
    pub sec: f64,
    pub threads: usize,
    pub hash: u32,
    pub draw: BTreeMap<String, String>,
    pub multipv: Vec<Pv>,
    pub bestmove: String,
}

impl Reading {
    pub fn lines(&self) -> &[Pv] {
        if self.bestmove == "win" {
            &[]
        } else {
            &self.multipv
        }
    }

    pub fn validate(&self, settings: &Settings) -> Result<()> {
        if self.nodes_limit != settings.nodes
            || self.threads != 1
            || self.hash != settings.hash
            || self.draw != settings.draw()
        {
            bail!("宣言と読みの条件が矛盾します: {}", self.key);
        }
        if self.ply == 0 || !self.sec.is_finite() || self.sec < 0.0 {
            bail!("不正な読み: {}", self.key);
        }
        let pos = position_from_sfen(&format!("{} {}", self.key, self.ply))?;
        if strip_ply(&pos.to_sfen()) != self.key {
            bail!("局面 key は正規形が必要です: {}", self.key);
        }
        if self.multipv.len() > settings.multipv {
            bail!("MultiPV が指定数を超えています: {}", self.key);
        }
        if self.bestmove == "win"
            && (self.multipv.is_empty()
                || matches!(self.multipv.as_slice(), [pv] if pv.mv == "win"))
        {
            return Ok(());
        }
        let mut moves = BTreeSet::new();
        for pv in &self.multipv {
            if !moves.insert(&pv.mv) {
                bail!("MultiPV の手が重複しています: {}", self.key);
            }
            child_position_after_move(&pos.to_sfen(), &pv.mv)?;
        }
        if self
            .multipv
            .first()
            .map_or(!matches!(self.bestmove.as_str(), "resign" | "win"), |p| p.mv != self.bestmove)
        {
            bail!("bestmove と MultiPV が矛盾: {}", self.key);
        }
        Ok(())
    }

    pub fn validate_declaration(&self, settings: &Settings) -> Result<()> {
        self.validate(settings)?;
        if self.multipv.len() != settings.multipv {
            bail!("宣言と MultiPV 行数が矛盾します: {}", self.key);
        }
        Ok(())
    }

    fn same_values(&self, other: &Self) -> bool {
        self.multipv == other.multipv && self.bestmove == other.bestmove
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    kind: String,
    settings: Settings,
    provenance: String,
    reading: Reading,
}

pub(super) struct LockedFile(pub File);
impl LockedFile {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock().with_context(|| format!("lock 取得失敗: {}", path.display()))?;
        Ok(Self(file))
    }
}
impl Drop for LockedFile {
    fn drop(&mut self) {
        if let Err(err) = self.0.unlock() {
            eprintln!("unlock: {err}");
        }
    }
}

pub(super) trait Readings {
    fn get(&self, key: &str) -> Option<&Reading>;
}

impl Readings for BTreeMap<String, Reading> {
    fn get(&self, key: &str) -> Option<&Reading> {
        self.get(key)
    }
}

// 1 件ごとには読みだけを持つ。条件の同一性は指紋で決まる (ファイル型 option はパスでなく
// 内容の sha256 で指紋に入るので、同じモデルを別のパスで指定した読みも同じ条件として使い回す)。
pub(super) struct Journal {
    lock: LockedFile,
    fingerprint: String,
    records: BTreeMap<String, Reading>,
}

impl Readings for Journal {
    fn get(&self, key: &str) -> Option<&Reading> {
        self.records.get(key)
    }
}

fn scan(lock: &mut LockedFile, mut visit: impl FnMut(Record) -> Result<()>) -> Result<()> {
    lock.0.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(lock.0.try_clone()?);
    let mut offset = 0;
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let n = reader.read_until(b'\n', &mut bytes)?;
        if n == 0 {
            break;
        }
        // 台帳は改行まで同期して保存する。末尾の未確定行だけを回復する。
        if !bytes.ends_with(b"\n") {
            lock.0.set_len(offset)?;
            break;
        }
        offset += n as u64;
        let rec: Record = serde_json::from_slice(&bytes)
            .context("tree 台帳の形式が不正です (既存 book_mine 台帳とは共用できません)")?;
        if rec.kind != VERSION || rec.settings.version != VERSION {
            bail!("台帳の版が違います");
        }
        visit(rec)?;
    }
    lock.0.seek(SeekFrom::End(0))?;
    Ok(())
}

fn conditions(lock: &mut LockedFile) -> Result<BTreeMap<String, Settings>> {
    let mut conditions = BTreeMap::new();
    scan(lock, |rec| {
        conditions.insert(rec.settings.fingerprint.clone(), rec.settings);
        Ok(())
    })?;
    Ok(conditions)
}

impl Journal {
    pub fn open(path: &Path, fingerprint: &str) -> Result<Self> {
        super::paths::reject_symlink(path)?;
        Self::load(LockedFile::open(path)?, fingerprint)
    }

    fn load(mut lock: LockedFile, fingerprint: &str) -> Result<Self> {
        let mut records: BTreeMap<String, Reading> = BTreeMap::new();
        scan(&mut lock, |rec| {
            if rec.settings.fingerprint != fingerprint {
                return Ok(());
            }
            rec.reading.validate(&rec.settings)?;
            if let Some(old) = records.get(&rec.reading.key)
                && !old.same_values(&rec.reading)
            {
                bail!("台帳の重複が矛盾します");
            }
            records.insert(rec.reading.key.clone(), rec.reading);
            Ok(())
        })?;
        Ok(Self {
            lock,
            fingerprint: fingerprint.into(),
            records,
        })
    }

    pub fn select(mut lock: LockedFile, fingerprint: Option<&str>) -> Result<(Self, Settings)> {
        let conditions = conditions(&mut lock)?;
        let settings = if let Some(fp) = fingerprint {
            conditions.get(fp).context("指紋が台帳にありません")?
        } else {
            if conditions.len() != 1 {
                bail!("台帳の条件が一意でありません。--fingerprint を指定してください");
            }
            conditions.values().next().context("空の台帳")?
        };
        let journal = Self::load(lock, &settings.fingerprint)?;
        Ok((journal, settings.clone()))
    }

    pub fn append(
        &mut self,
        reading: Reading,
        settings: &Settings,
        provenance: String,
    ) -> Result<()> {
        if settings.fingerprint != self.fingerprint {
            bail!("台帳を開いた指紋と追記の条件が違います");
        }
        reading.validate(settings)?;
        let key = reading.key.clone();
        if let Some(old) = self.records.get(&key) {
            if !old.same_values(&reading) {
                bail!("値の違う重複: {}", reading.key);
            }
            return Ok(());
        }
        let rec = Record {
            kind: VERSION.into(),
            settings: settings.clone(),
            provenance,
            reading,
        };
        serde_json::to_writer(&mut self.lock.0, &rec)?;
        self.lock.0.write_all(b"\n")?;
        self.lock.0.sync_data()?;
        self.records.insert(key, rec.reading);
        Ok(())
    }

    pub fn import(&mut self, paths: &[PathBuf], settings: &Settings) -> Result<()> {
        if settings.fingerprint != self.fingerprint {
            bail!("台帳を開いた指紋と取り込みの条件が違います");
        }
        let mut incoming = Vec::new();
        for path in paths {
            let provenance = format!("declared-python:{}:sha256={}", path.display(), digest(path)?);
            for (line_no, line) in BufReader::new(File::open(path)?).lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let reading: Reading = serde_json::from_str(&line)
                    .with_context(|| format!("{}:{}", path.display(), line_no + 1))?;
                reading.validate_declaration(settings)?;
                incoming.push((reading, provenance.clone()));
            }
        }
        self.append_all(incoming, settings)?;
        Ok(())
    }

    /// 全件を検査してから追記し、拒否時には台帳を変更しない。値の同じ重複は飛ばし、値の違う重複は
    /// 全体を拒否する。(追記した件数, 飛ばした件数) を返す。
    pub fn append_all(
        &mut self,
        incoming: Vec<(Reading, String)>,
        settings: &Settings,
    ) -> Result<(usize, usize)> {
        if settings.fingerprint != self.fingerprint {
            bail!("台帳を開いた指紋と取り込みの条件が違います");
        }
        let total = incoming.len();
        let mut pending: BTreeMap<String, (Reading, String)> = BTreeMap::new();
        for (reading, provenance) in incoming {
            reading.validate(settings)?;
            let existing = self
                .records
                .get(&reading.key)
                .or_else(|| pending.get(&reading.key).map(|(r, _)| r));
            if let Some(old) = existing
                && !old.same_values(&reading)
            {
                bail!("値の違う重複: {}", reading.key);
            }
            pending.insert(reading.key.clone(), (reading, provenance));
        }
        let mut appended = 0;
        for (reading, provenance) in pending.into_values() {
            if !self.records.contains_key(&reading.key) {
                appended += 1;
            }
            self.append(reading, settings, provenance)?;
        }
        Ok((appended, total - appended))
    }
}

/// 取り込み元の台帳から選んだ 1 条件の読み。
pub(super) struct Source {
    pub settings: Settings,
    pub readings: Vec<Reading>,
    /// 読んだ完全な行 (末尾の未確定行を除く) の SHA-256。
    pub sha256: String,
    /// 改行で終わらない末尾の行を無視したか。
    pub ignored_tail: bool,
}

/// 取り込み元の台帳を読む。共有 lock を取って書き手 (serve 等の排他 lock) と並ばないようにし、
/// ファイルは変更しない。条件の一覧を作ってから選んだ条件の読みだけを保持する。
pub(super) fn read_source(path: &Path, fingerprint: Option<&str>) -> Result<Source> {
    let file =
        File::open(path).with_context(|| format!("取り込み元を開けません: {}", path.display()))?;
    file.try_lock_shared().with_context(|| {
        format!("取り込み元の台帳を書き込み中のプロセスがあります: {}", path.display())
    })?;
    let mut reader = BufReader::new(file);
    let mut hash = Sha256::new();
    let mut complete = 0u64;
    let mut ignored_tail = false;
    let mut conditions: BTreeMap<String, Settings> = BTreeMap::new();
    for_each_complete_line(
        &mut reader,
        u64::MAX,
        |bytes| {
            hash.update(bytes);
            complete += bytes.len() as u64;
            let rec = parse_record(bytes)?;
            conditions.entry(rec.settings.fingerprint.clone()).or_insert(rec.settings);
            Ok(())
        },
        &mut ignored_tail,
    )?;
    if conditions.is_empty() {
        bail!("取り込み元の台帳が空です: {}", path.display());
    }
    let mut settings = match fingerprint {
        Some(fp) => conditions.remove(fp).context("指紋が取り込み元の台帳にありません")?,
        None => {
            if conditions.len() != 1 {
                bail!("取り込み元の条件が一意でありません。--fingerprint を指定してください");
            }
            conditions.into_values().next().context("取り込み元の台帳が空です")?
        }
    };
    if settings.computed_fingerprint()? != settings.fingerprint {
        bail!("取り込み元の条件が指紋と合いません: {}", settings.fingerprint);
    }
    let mut file = reader.into_inner();
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut records: BTreeMap<String, Reading> = BTreeMap::new();
    for_each_complete_line(
        &mut reader,
        complete,
        |bytes| {
            let rec = parse_record(bytes)?;
            if rec.settings.fingerprint != settings.fingerprint {
                return Ok(());
            }
            // 指紋が同じでも条件の中身が違う行は、指紋の付け間違いか改変なので取り込まない。
            if !rec.settings.same_apart_from_version(&settings) {
                bail!("取り込み元に指紋と条件の合わない行があります: {}", rec.reading.key);
            }
            merge_engine_version(&mut settings, &rec.settings)?;
            rec.reading.validate(&rec.settings)?;
            if let Some(old) = records.get(&rec.reading.key)
                && !old.same_values(&rec.reading)
            {
                bail!("取り込み元の重複が矛盾します: {}", rec.reading.key);
            }
            records.insert(rec.reading.key.clone(), rec.reading);
            Ok(())
        },
        &mut false,
    )?;
    Ok(Source {
        settings,
        readings: records.into_values().collect(),
        sha256: format!("{:x}", hash.finalize()),
        ignored_tail,
    })
}

fn for_each_complete_line(
    reader: &mut impl BufRead,
    limit: u64,
    mut visit: impl FnMut(&[u8]) -> Result<()>,
    ignored_tail: &mut bool,
) -> Result<()> {
    let mut read = 0u64;
    let mut bytes = Vec::new();
    while read < limit {
        bytes.clear();
        let n = reader.read_until(b'\n', &mut bytes)?;
        if n == 0 {
            break;
        }
        if !bytes.ends_with(b"\n") {
            *ignored_tail = true;
            break;
        }
        read += n as u64;
        visit(&bytes)?;
    }
    Ok(())
}

fn parse_record(bytes: &[u8]) -> Result<Record> {
    let rec: Record = serde_json::from_slice(bytes).context("tree 台帳の形式が不正です")?;
    if rec.kind != VERSION || rec.settings.version != VERSION {
        bail!("台帳の版が違います");
    }
    Ok(rec)
}

/// 版を記録する前の行と後の行が同じ指紋に混ざる。指紋はバイナリの内容を含むので、分かった版を採る。
fn merge_engine_version(known: &mut Settings, other: &Settings) -> Result<()> {
    match (&known.engine_version, &other.engine_version) {
        (Some(a), Some(b)) if a != b => bail!("同じ指紋に違うエンジンの版があります: {a} / {b}"),
        (None, Some(_)) => known.engine_version = other.engine_version.clone(),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fingerprint: &str, column: usize) -> Record {
        let row = match column {
            0 => "K8".into(),
            8 => "8K".into(),
            n => format!("{n}K{}", 8 - n),
        };
        let settings = Settings {
            fingerprint: fingerprint.into(),
            version: VERSION.into(),
            engine_sha256: "engine".into(),
            options: BTreeMap::new(),
            content_sha256: BTreeMap::new(),
            nodes: 100,
            multipv: 1,
            hash: 16,
            engine_version: None,
        };
        let reading = Reading {
            tag: "fixture".into(),
            ply: 1,
            key: format!("4k4/9/9/9/9/9/9/9/{row} b -"),
            nodes_limit: settings.nodes,
            nodes: Some(100),
            sec: 0.1,
            threads: 1,
            hash: settings.hash,
            draw: BTreeMap::new(),
            multipv: Vec::new(),
            bestmove: "resign".into(),
        };
        Record {
            kind: VERSION.into(),
            settings,
            provenance: "fixture".into(),
            reading,
        }
    }

    fn write_record(file: &mut File, record: &Record) {
        serde_json::to_writer(&mut *file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn journal_retains_only_selected_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let target = record("target", 0);
        {
            let mut file = File::create(&path).unwrap();
            write_record(&mut file, &target);
            for condition in 0..128 {
                for column in 0..9 {
                    write_record(&mut file, &record(&format!("past-{condition}"), column));
                }
            }
            let mut conflict = record("past-0", 0);
            conflict.reading.bestmove = "win".into();
            write_record(&mut file, &conflict);
            file.write_all(b"{partial").unwrap();
        }
        let mut journal = Journal::open(&path, "target").unwrap();
        assert_eq!(journal.records.len(), 1);
        assert_eq!(journal.get(&target.reading.key), Some(&target.reading));
        assert!(Journal::open(&path, "past-1").is_err());
        let next = record("target", 1);
        journal.append(next.reading.clone(), &next.settings, "fixture".into()).unwrap();
        assert_eq!(journal.records.len(), 2);
        assert_eq!(journal.get(&next.reading.key), Some(&next.reading));
        let foreign = record("foreign", 2);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(journal.append(foreign.reading, &foreign.settings, "fixture".into()).is_err());
        assert!(journal.import(&[], &foreign.settings).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        drop(journal);
        assert!(Journal::open(&path, "past-0").is_err());
        assert_eq!(Journal::open(&path, "past-1").unwrap().records.len(), 9);
        assert!(Journal::open(&path, "absent").unwrap().records.is_empty());
        assert_eq!(Journal::open(&path, "target").unwrap().records.len(), 2);
    }

    #[test]
    fn same_model_under_another_path_shares_the_condition() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"model").unwrap();
        let moved = dir.path().join("moved");
        std::fs::create_dir(&moved).unwrap();
        std::fs::copy(&model, moved.join("model.bin")).unwrap();
        let engine = dir.path().join("engine");
        std::fs::write(&engine, b"engine").unwrap();
        let settings_at = |eval: &Path| {
            Settings::new(&crate::book_tree::ReadingArgs {
                engine: engine.clone(),
                engine_options: vec![format!("EvalFile={}", eval.display())],
                nodes: 100,
                multipv: 1,
                hash: 16,
            })
            .unwrap()
        };
        let first = settings_at(&model);
        let second = settings_at(&moved.join("model.bin"));
        assert_ne!(first.options, second.options);
        assert_eq!(first.fingerprint, second.fingerprint);
        let path = dir.path().join("journal.jsonl");
        let reading_for = |column| {
            let mut r = record(&first.fingerprint, column).reading;
            r.draw = first.draw();
            r
        };
        let (a, b) = (reading_for(0), reading_for(1));
        let mut journal = Journal::open(&path, &first.fingerprint).unwrap();
        journal.append(a, &first, "fixture".into()).unwrap();
        journal.append(b, &second, "fixture".into()).unwrap();
        drop(journal);
        let journal = Journal::open(&path, &second.fingerprint).unwrap();
        assert_eq!(journal.records.len(), 2);
    }

    #[test]
    fn streaming_select_lists_settings_and_preserves_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let first = record("first", 0);
        let mut second = record("second", 1);
        second.settings.engine_sha256 = "other-engine".into();
        {
            let mut file = File::create(&path).unwrap();
            for rec in [&first, &second, &first] {
                write_record(&mut file, rec);
            }
        }
        let mut lock = LockedFile::open(&path).unwrap();
        let found = conditions(&mut lock).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found["first"], first.settings);
        assert_eq!(found["second"], second.settings);
        assert!(Journal::select(lock, None).is_err());
        assert!(Journal::select(LockedFile::open(&path).unwrap(), Some("absent")).is_err());
        let (journal, settings) =
            Journal::select(LockedFile::open(&path).unwrap(), Some("second")).unwrap();
        assert_eq!(settings, second.settings);
        assert_eq!(journal.records.len(), 1);
        assert_eq!(journal.get(&second.reading.key), Some(&second.reading));
        assert!(LockedFile::open(&path).is_err());
        drop(journal);
        {
            let mut file = File::create(&path).unwrap();
            write_record(&mut file, &first);
            write_record(&mut file, &first);
            file.write_all(b"{partial").unwrap();
        }
        let (journal, settings) = Journal::select(LockedFile::open(&path).unwrap(), None).unwrap();
        assert_eq!(settings, first.settings);
        assert_eq!(journal.records.len(), 1);
        drop(journal);
        File::create(&path).unwrap();
        assert!(Journal::select(LockedFile::open(&path).unwrap(), None).is_err());
    }

    #[test]
    fn unselected_rows_check_format_and_version_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut invalid_version = record("past", 0);
        invalid_version.settings.version = "unknown".into();
        {
            let mut file = File::create(&path).unwrap();
            write_record(&mut file, &invalid_version);
        }
        assert!(Journal::open(&path, "target").is_err());
        assert!(Journal::select(LockedFile::open(&path).unwrap(), None).is_err());
        let mut invalid_reading = record("past", 0);
        invalid_reading.reading.threads = 2;
        {
            let mut file = File::create(&path).unwrap();
            write_record(&mut file, &invalid_reading);
        }
        assert!(Journal::open(&path, "target").unwrap().records.is_empty());
        assert!(Journal::open(&path, "past").is_err());
        assert!(Journal::select(LockedFile::open(&path).unwrap(), None).is_err());
        std::fs::write(&path, b"{invalid}\n").unwrap();
        assert!(Journal::open(&path, "target").is_err());
    }
}
