use super::ReadingArgs;
use crate::book_db::{child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail};
use rshogi_core::movegen::{MoveList, generate_legal_all};
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
        };
        let mut normalized = settings.clone();
        for (k, hash) in &settings.content_sha256 {
            if normalized.options.contains_key(k) {
                normalized.options.insert(k.clone(), format!("sha256:{hash}"));
            }
        }
        settings.fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&normalized)?));
        Ok(settings)
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
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        if self.multipv.len() != settings.multipv.min(legal.len()) {
            bail!("合法手数と MultiPV が矛盾: {}", self.key);
        }
        for pv in &self.multipv {
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

pub(super) struct Journal {
    lock: LockedFile,
    records: BTreeMap<(String, String), Record>,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let mut lock = LockedFile::open(path)?;
        let mut reader = BufReader::new(lock.0.try_clone()?);
        let mut records: BTreeMap<(String, String), Record> = BTreeMap::new();
        let mut offset = 0;
        loop {
            let mut bytes = Vec::new();
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
            rec.reading.validate(&rec.settings)?;
            let key = (rec.settings.fingerprint.clone(), rec.reading.key.clone());
            if let Some(old) = records.get(&key)
                && (!old.reading.same_values(&rec.reading) || old.settings != rec.settings)
            {
                bail!("台帳の重複が矛盾します");
            }
            records.insert(key, rec);
        }
        lock.0.seek(SeekFrom::End(0))?;
        Ok(Self { lock, records })
    }

    pub fn records_for(&self, fingerprint: &str) -> BTreeMap<String, Reading> {
        self.records
            .iter()
            .filter(|((fp, _), _)| fp == fingerprint)
            .map(|((_, key), r)| (key.clone(), r.reading.clone()))
            .collect()
    }

    pub fn select(&self, fingerprint: Option<&str>) -> Result<Settings> {
        let conditions: BTreeMap<_, _> = self
            .records
            .values()
            .map(|r| (r.settings.fingerprint.as_str(), &r.settings))
            .collect();
        if let Some(fp) = fingerprint {
            return conditions.get(fp).map(|s| (*s).clone()).context("指紋が台帳にありません");
        }
        if conditions.len() != 1 {
            bail!("台帳の条件が一意でありません。--fingerprint を指定してください");
        }
        conditions.values().next().map(|s| (*s).clone()).context("空の台帳")
    }

    pub fn append(
        &mut self,
        reading: Reading,
        settings: &Settings,
        provenance: String,
    ) -> Result<()> {
        reading.validate(settings)?;
        let key = (settings.fingerprint.clone(), reading.key.clone());
        if let Some(old) = self.records.get(&key) {
            if !old.reading.same_values(&reading) {
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
        self.records.insert(key, rec);
        Ok(())
    }

    pub fn import(&mut self, paths: &[PathBuf], settings: &Settings) -> Result<()> {
        // 取り込み全体を検査してから追記し、拒否時には台帳を変更しない。
        let mut pending: BTreeMap<String, (Reading, String)> = BTreeMap::new();
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
                let existing = self
                    .records
                    .get(&(settings.fingerprint.clone(), reading.key.clone()))
                    .map(|r| &r.reading)
                    .or_else(|| pending.get(&reading.key).map(|(r, _)| r));
                if let Some(old) = existing
                    && !old.same_values(&reading)
                {
                    bail!("値の違う重複: {}", reading.key);
                }
                pending.insert(reading.key.clone(), (reading, provenance.clone()));
            }
        }
        for (reading, provenance) in pending.into_values() {
            self.append(reading, settings, provenance)?;
        }
        Ok(())
    }
}
