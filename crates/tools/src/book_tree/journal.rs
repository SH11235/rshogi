use super::ReadingArgs;
use super::file_lock;
use crate::book_db::{child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

    /// 木ごとの探索幅とノード数を指定した条件を返す。
    pub fn with_overrides(&self, multipv: usize, nodes: u64) -> Result<Self> {
        if multipv == 0 || nodes == 0 {
            bail!("nodes / multipv は正数が必要です");
        }
        let mut settings = self.clone();
        settings.multipv = multipv;
        settings.nodes = nodes;
        settings.options.insert("MultiPV".into(), multipv.to_string());
        settings.fingerprint = settings.computed_fingerprint()?;
        Ok(settings)
    }

    /// 条件の指紋。ファイル型 option はパスでなく内容の SHA-256 で入れる。
    pub fn computed_fingerprint(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&self.normalized())?)))
    }

    /// 指紋の元になる形。版は入れない (入れると版を記録する前の台帳と指紋が変わる)。
    fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.fingerprint = String::new();
        normalized.engine_version = None;
        for (k, hash) in &self.content_sha256 {
            if normalized.options.contains_key(k) {
                normalized.options.insert(k.clone(), format!("sha256:{hash}"));
            }
        }
        normalized
    }

    /// 版とファイル型 option のパスを除いて同じ条件か (モデルを移しても serve は同じ指紋で追記する)。
    pub fn same_condition(&self, other: &Self) -> bool {
        self.normalized() == other.normalized()
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

    /// 手数・計測情報を除く、台帳の重複判定に使う値の一致。
    pub(super) fn same_values(&self, other: &Self) -> bool {
        self.multipv == other.multipv && self.bestmove == other.bestmove
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// 台帳に保存する条件付きの読み。
pub(super) struct Record {
    pub kind: String,
    pub settings: Settings,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prefetch: bool,
    pub reading: Reading,
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
        file_lock::try_lock(&file).with_context(|| format!("lock 取得失敗: {}", path.display()))?;
        Ok(Self(file))
    }
}
impl Drop for LockedFile {
    fn drop(&mut self) {
        if let Err(err) = file_lock::unlock(&self.0) {
            eprintln!("unlock: {err}");
        }
    }
}

/// 閉じるだけでは、同時に fork された子プロセスが同じファイル記述を持つ間ロックが残るので明示的に外す。
struct UnlockOnDrop(File);
impl Drop for UnlockOnDrop {
    fn drop(&mut self) {
        if let Err(err) = file_lock::unlock(&self.0) {
            eprintln!("unlock: {err}");
        }
    }
}

pub(super) trait Readings {
    fn get(&self, key: &str) -> Option<&Reading>;
    fn get_preferred(&self, key: &str, preferred: Option<&str>) -> Option<&Reading>;
    fn condition(&self, key: &str, preferred: Option<&str>) -> Option<&str>;
    fn has_exact(&self, key: &str, fingerprint: &str) -> bool {
        self.condition(key, Some(fingerprint)) == Some(fingerprint)
    }
}

impl Readings for BTreeMap<String, Reading> {
    fn get(&self, key: &str) -> Option<&Reading> {
        self.get(key)
    }
    fn get_preferred(&self, key: &str, _: Option<&str>) -> Option<&Reading> {
        self.get(key)
    }
    fn condition(&self, _: &str, _: Option<&str>) -> Option<&str> {
        None
    }
}

/// 自条件の読みが無い場合の優先順位。追記順に依存させない。
pub(super) fn compare_readings(
    a: &Reading,
    a_fp: &str,
    b: &Reading,
    b_fp: &str,
) -> std::cmp::Ordering {
    a.nodes_limit
        .cmp(&b.nodes_limit)
        .then_with(|| a.multipv.len().cmp(&b.multipv.len()))
        .then_with(|| b_fp.cmp(a_fp))
}

type CondId = usize;

enum Rows {
    One((CondId, Reading)),
    Many(Vec<(CondId, Reading)>),
}

impl Rows {
    fn as_slice(&self) -> &[(CondId, Reading)] {
        match self {
            Self::One(row) => std::slice::from_ref(row),
            Self::Many(rows) => rows,
        }
    }

    fn insert(&mut self, id: CondId, reading: Reading) {
        *self = match std::mem::replace(self, Self::Many(Vec::new())) {
            Self::One(old) if old.0 == id => Self::One(old),
            Self::One(old) => Self::Many(vec![old, (id, reading)]),
            Self::Many(mut rows) => {
                if !rows.iter().any(|(old_id, _)| *old_id == id) {
                    rows.push((id, reading));
                }
                Self::Many(rows)
            }
        };
    }
}

#[derive(Default, serde::Serialize)]
pub(super) struct SkippedRows {
    invalid: usize,
    conflicting: usize,
}

pub(super) struct Journal {
    lock: LockedFile,
    fingerprint: String,
    records: BTreeMap<String, Rows>,
    conditions: Vec<Arc<Settings>>,
    condition_ids: BTreeMap<String, CondId>,
    prefetch_conditions: BTreeSet<(CondId, String)>,
    pub skipped_rows: BTreeMap<String, SkippedRows>,
    pub prefetched: BTreeSet<String>,
    /// エンジンで読んだ直近の行の、1 ノードあたりの秒数。
    pub durations: VecDeque<f64>,
}

impl Readings for Journal {
    fn get(&self, key: &str) -> Option<&Reading> {
        self.get_preferred(key, Some(&self.fingerprint))
    }
    fn get_preferred(&self, key: &str, preferred: Option<&str>) -> Option<&Reading> {
        self.selected(key, preferred).map(|(_, reading)| reading)
    }
    fn condition(&self, key: &str, preferred: Option<&str>) -> Option<&str> {
        self.selected(key, preferred)
            .map(|(id, _)| self.conditions[*id].fingerprint.as_str())
    }
}

fn scan(file: &mut File, mut visit: impl FnMut(Record) -> Result<()>) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file.try_clone()?);
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
            file.set_len(offset)?;
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
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

impl Journal {
    pub fn open(path: &Path, fingerprint: &str) -> Result<Self> {
        super::paths::reject_symlink(path)?;
        Self::load(LockedFile::open(path)?, fingerprint)
    }

    fn load(lock: LockedFile, fingerprint: &str) -> Result<Self> {
        let mut journal = Self {
            lock,
            fingerprint: fingerprint.into(),
            records: BTreeMap::new(),
            conditions: Vec::new(),
            condition_ids: BTreeMap::new(),
            prefetch_conditions: BTreeSet::new(),
            skipped_rows: BTreeMap::new(),
            prefetched: BTreeSet::new(),
            durations: VecDeque::new(),
        };
        // scan の間だけファイルを分離し、行を複製せず索引へ移す。
        let mut scan_file = journal.lock.0.try_clone()?;
        let mut validity = Validity::default();
        scan(&mut scan_file, |rec| {
            if !validity.valid(&rec)? {
                journal.skipped_rows.entry(rec.settings.fingerprint).or_default().invalid += 1;
                return Ok(());
            }
            if journal
                .get_exact(&rec.reading.key, &rec.settings.fingerprint)
                .is_some_and(|old| !old.same_values(&rec.reading))
            {
                journal.skipped_rows.entry(rec.settings.fingerprint).or_default().conflicting += 1;
                return Ok(());
            }
            journal.remember(rec);
            Ok(())
        })?;
        report_skipped(&journal.skipped_rows);
        Ok(journal)
    }

    fn condition_id(&mut self, settings: &Settings) -> CondId {
        *self.condition_ids.entry(settings.fingerprint.clone()).or_insert_with(|| {
            let id = self.conditions.len();
            self.conditions.push(Arc::new(settings.clone()));
            id
        })
    }

    fn remember(&mut self, rec: Record) {
        let id = self.condition_id(&rec.settings);
        merge_engine_version(Arc::make_mut(&mut self.conditions[id]), &rec.settings);
        let key = rec.reading.key.clone();
        if rec.prefetch {
            self.prefetched.insert(key.clone());
            self.prefetch_conditions.insert((id, key.clone()));
        }
        if rec.provenance == "engine" && rec.reading.nodes_limit > 0 {
            self.durations.push_back(rec.reading.sec / rec.reading.nodes_limit as f64);
            if self.durations.len() > 50 {
                self.durations.pop_front();
            }
        }
        match self.records.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Rows::One((id, rec.reading)));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().insert(id, rec.reading)
            }
        }
    }

    fn selected(&self, key: &str, preferred: Option<&str>) -> Option<&(CondId, Reading)> {
        let rows = self.records.get(key)?.as_slice();
        preferred
            .and_then(|fp| self.condition_ids.get(fp))
            .and_then(|id| rows.iter().find(|(s, _)| s == id))
            .or_else(|| {
                rows.iter().max_by(|a, b| {
                    compare_readings(
                        &a.1,
                        &self.conditions[a.0].fingerprint,
                        &b.1,
                        &self.conditions[b.0].fingerprint,
                    )
                })
            })
    }

    /// 指定した条件と完全に一致する読みだけを返す。
    pub fn get_exact(&self, key: &str, fingerprint: &str) -> Option<&Reading> {
        let id = self.condition_ids.get(fingerprint)?;
        self.records
            .get(key)?
            .as_slice()
            .iter()
            .find(|(s, _)| s == id)
            .map(|(_, reading)| reading)
    }

    pub fn select(lock: LockedFile, fingerprint: Option<&str>) -> Result<(Self, Option<Settings>)> {
        let journal = Self::load(lock, fingerprint.unwrap_or(""))?;
        let settings = if let Some(fp) = fingerprint {
            let id = journal.condition_ids.get(fp).context("指紋が台帳にありません")?;
            Some((*journal.conditions[*id]).clone())
        } else {
            if journal.lock.0.metadata()?.len() == 0 {
                bail!("空の台帳");
            }
            None
        };
        Ok((journal, settings))
    }

    #[cfg(test)]
    pub fn append(
        &mut self,
        reading: Reading,
        settings: &Settings,
        provenance: String,
    ) -> Result<()> {
        self.append_marked(reading, settings, provenance, false)
    }

    pub fn append_marked(
        &mut self,
        reading: Reading,
        settings: &Settings,
        provenance: String,
        prefetch: bool,
    ) -> Result<()> {
        self.append_unsynced(reading, settings, provenance, prefetch)?;
        self.lock.0.sync_data()?;
        Ok(())
    }

    fn append_unsynced(
        &mut self,
        reading: Reading,
        settings: &Settings,
        provenance: String,
        prefetch: bool,
    ) -> Result<()> {
        Validity::default().require(&reading, settings)?;
        let key = reading.key.clone();
        let reading = if let Some(old) = self.get_exact(&key, &settings.fingerprint) {
            if !old.same_values(&reading) {
                bail!("値の違う重複: {}", reading.key);
            }
            let id = self.condition_ids[&settings.fingerprint];
            if !prefetch || self.prefetch_conditions.contains(&(id, key.clone())) {
                return Ok(());
            }
            // 同値の再追記でも、先に確定した手数と読みは保つ。
            old.clone()
        } else {
            reading
        };
        let rec = Record {
            kind: VERSION.into(),
            settings: settings.clone(),
            provenance,
            prefetch,
            reading,
        };
        let mut bytes = serde_json::to_vec(&rec)?;
        bytes.push(b'\n');
        self.lock.0.write_all(&bytes)?;
        self.remember(rec);
        Ok(())
    }

    /// 全件を検査してから追記し、拒否時には台帳を変更しない。値の同じ重複は飛ばし、値の違う重複は `on_conflict` に従う。
    /// `Reject` なら全体を拒否し、`KeepExisting` なら先にある値 (台帳の値、無ければ取り込みの先の行) を残して、
    /// 残さなかった読みを検査しながら `sink` に渡す (食い違いを全件メモリに持たない)。
    /// `sink.before_append` は検査の後・追記の前に呼び、失敗すれば何も追記しない。
    /// `prefetched` の局面は先読みの印を付けて追記する (取り込み元の印を引き継ぐ)。
    /// (追記した件数, 飛ばした件数, 食い違いの件数) を返す。飛ばした件数は食い違いを含む。
    #[cfg(test)]
    pub fn merge_all(
        &mut self,
        incoming: Vec<(Reading, String)>,
        settings: &Settings,
        on_conflict: OnConflict,
        sink: &mut dyn ConflictSink,
        prefetched: &BTreeSet<String>,
    ) -> Result<(usize, usize, usize)> {
        let settings = Arc::new(settings.clone());
        self.merge_records(
            incoming.into_iter().map(|(r, p)| (r, Arc::clone(&settings), p)).collect(),
            on_conflict,
            sink,
            &prefetched
                .iter()
                .map(|key| (settings.fingerprint.clone(), key.clone()))
                .collect(),
        )
    }

    /// 各行の読み条件と入力順を保って取り込む。食い違いは同じ条件・局面の間だけで判定する。
    pub fn merge_records(
        &mut self,
        incoming: Vec<(Reading, Arc<Settings>, String)>,
        on_conflict: OnConflict,
        sink: &mut dyn ConflictSink,
        prefetched: &BTreeSet<(String, String)>,
    ) -> Result<(usize, usize, usize)> {
        let total = incoming.len();
        let mut pending = Vec::new();
        let mut indices = BTreeMap::new();
        let mut conflicts = 0;
        let mut validity = Validity::default();
        for (reading, settings, provenance) in incoming {
            validity.require(&reading, &settings)?;
            let identity = (reading.key.clone(), settings.fingerprint.clone());
            let existing = self.get_exact(&reading.key, &settings.fingerprint).or_else(|| {
                indices.get(&identity).map(|&index: &usize| {
                    let (old, _, _): &(Reading, Arc<Settings>, String) = &pending[index];
                    old
                })
            });
            if let Some(old) = existing
                && !old.same_values(&reading)
            {
                match on_conflict {
                    OnConflict::Reject => bail!("値の違う重複: {}", reading.key),
                    OnConflict::KeepExisting => {
                        sink.conflict(old, &reading, &provenance)?;
                        conflicts += 1;
                        continue;
                    }
                }
            }
            if let std::collections::btree_map::Entry::Vacant(entry) = indices.entry(identity) {
                entry.insert(pending.len());
                pending.push((reading, settings, provenance));
            }
        }
        sink.before_append()?;
        let mut appended = 0;
        let mut wrote = false;
        for (reading, settings, provenance) in pending {
            if self.get_exact(&reading.key, &settings.fingerprint).is_none() {
                appended += 1;
            }
            let prefetch =
                prefetched.contains(&(settings.fingerprint.clone(), reading.key.clone()));
            self.append_unsynced(reading, &settings, provenance, prefetch)?;
            wrote = true;
        }
        if wrote {
            self.lock.0.sync_data()?;
        }
        Ok((appended, total - appended, conflicts))
    }
}

/// 値の違う重複の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum OnConflict {
    /// 1 件でもあれば取り込み全体を拒否する
    Reject,
    /// 先にある値を残し、食い違いを一覧に出す
    KeepExisting,
}

/// `merge_all` が残さなかった読みの受け取り先。
pub(super) trait ConflictSink {
    /// 残した読み・残さなかった読み・残さなかった読みの出どころを 1 件受け取る
    fn conflict(&mut self, existing: &Reading, incoming: &Reading, provenance: &str) -> Result<()>;
    /// 検査の後・追記の前に呼ぶ。失敗すれば何も追記しない
    fn before_append(&mut self) -> Result<()>;
}

/// 食い違いを受け取らない (`OnConflict::Reject` で使う)。
pub(super) struct NoConflictList;

impl ConflictSink for NoConflictList {
    fn conflict(&mut self, _: &Reading, incoming: &Reading, _: &str) -> Result<()> {
        bail!("値の違う重複: {}", incoming.key)
    }

    fn before_append(&mut self) -> Result<()> {
        Ok(())
    }
}

/// 取り込み元の台帳から選んだ読み。
pub(super) struct Source {
    pub readings: Vec<Reading>,
    pub reading_settings: Vec<Arc<Settings>>,
    pub prefetched: BTreeSet<(String, String)>,
    /// 読んだ完全な行 (末尾の未確定行を除く) の SHA-256。
    pub sha256: String,
    /// 改行で終わらない末尾の行を無視したか。
    pub ignored_tail: bool,
}

impl Source {
    /// 局面ごとに、指定条件を優先し、無ければノード数・PV 行数・指紋で選ぶ。
    pub fn selected(&self, preferred: Option<&str>) -> BTreeMap<&str, (&Reading, &Settings)> {
        let mut selected: BTreeMap<&str, (&Reading, &Settings)> = BTreeMap::new();
        for (reading, settings) in self.readings.iter().zip(&self.reading_settings) {
            let entry = selected.entry(&reading.key).or_insert((reading, settings));
            let rank = |s: &Settings| preferred == Some(s.fingerprint.as_str());
            if rank(settings)
                .cmp(&rank(entry.1))
                .then_with(|| {
                    compare_readings(reading, &settings.fingerprint, entry.0, &entry.1.fingerprint)
                })
                .is_gt()
            {
                *entry = (reading, settings);
            }
        }
        selected
    }
}

/// 取り込み元の台帳を読む。共有 lock を取って書き手 (serve 等の排他 lock) と並ばないようにし、
/// ファイルは変更しない。条件の一覧を作ってから選んだ条件の読みのうち、`keep` が真の局面だけを保持する
/// (全行の検査はする)。
pub(super) fn read_source(
    path: &Path,
    fingerprint: Option<&str>,
    keep: impl Fn(&str) -> bool,
) -> Result<Source> {
    let file =
        File::open(path).with_context(|| format!("取り込み元を開けません: {}", path.display()))?;
    file_lock::try_lock_shared(&file).with_context(|| {
        format!("取り込み元の台帳を書き込み中のプロセスがあります: {}", path.display())
    })?;
    let _unlock = UnlockOnDrop(file.try_clone()?);
    let mut reader = BufReader::new(file);
    let mut hash = Sha256::new();
    let mut complete = 0u64;
    let mut ignored_tail = false;
    let mut found_fingerprint = fingerprint.is_none();
    let mut conditions: BTreeMap<String, Settings> = BTreeMap::new();
    for_each_complete_line(
        &mut reader,
        u64::MAX,
        |bytes| {
            hash.update(bytes);
            complete += bytes.len() as u64;
            let rec = parse_record(bytes)?;
            found_fingerprint |= fingerprint == Some(rec.settings.fingerprint.as_str());
            Ok(())
        },
        &mut ignored_tail,
    )?;
    if complete == 0 {
        bail!("取り込み元の台帳が空です: {}", path.display());
    }
    if !found_fingerprint {
        bail!("指紋が取り込み元の台帳にありません");
    }
    let mut file = reader.into_inner();
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut readings = Vec::new();
    let mut reading_settings = Vec::new();
    let mut shared: BTreeMap<String, (CondId, Arc<Settings>)> = BTreeMap::new();
    let mut indices = BTreeMap::new();
    let mut prefetched = BTreeSet::new();
    let mut seen = HashMap::new();
    let mut skipped_rows = BTreeMap::<String, SkippedRows>::new();
    let mut validity = Validity::default();
    for_each_complete_line(
        &mut reader,
        complete,
        |bytes| {
            let rec = parse_record(bytes)?;
            if fingerprint.is_some_and(|fp| rec.settings.fingerprint != fp) {
                return Ok(());
            }
            if !validity.valid(&rec)? {
                skipped_rows.entry(rec.settings.fingerprint).or_default().invalid += 1;
                return Ok(());
            }
            if !remember_values(&mut seen, &rec.reading, &rec.settings.fingerprint)? {
                skipped_rows.entry(rec.settings.fingerprint).or_default().conflicting += 1;
                return Ok(());
            }
            if !keep(&rec.reading.key) {
                return Ok(());
            }
            let known = conditions
                .entry(rec.settings.fingerprint.clone())
                .or_insert_with(|| rec.settings.clone());
            merge_engine_version(known, &rec.settings);
            let next_id = shared.len();
            let (id, condition) = shared
                .entry(rec.settings.fingerprint.clone())
                .or_insert_with(|| (next_id, Arc::new(known.clone())));
            let identity = (rec.reading.key.clone(), *id);
            if rec.prefetch {
                prefetched.insert((rec.settings.fingerprint.clone(), rec.reading.key.clone()));
            }
            if let Some(&index) = indices.get(&identity) {
                let old: &Reading = &readings[index];
                debug_assert!(old.same_values(&rec.reading));
            } else {
                indices.insert(identity, readings.len());
                reading_settings.push(Arc::clone(condition));
                readings.push(rec.reading);
            }
            Ok(())
        },
        &mut false,
    )?;
    report_skipped(&skipped_rows);
    for (fp, (_, settings)) in &mut shared {
        *settings = Arc::new(conditions[fp].clone());
    }
    for settings in &mut reading_settings {
        *settings = Arc::clone(&shared[&settings.fingerprint].1);
    }
    Ok(Source {
        readings,
        reading_settings,
        prefetched,
        sha256: format!("{:x}", hash.finalize()),
        ignored_tail,
    })
}

#[cfg(test)]
pub(super) fn simulation_readings(
    path: &Path,
    fingerprint: &str,
) -> Result<BTreeMap<String, Reading>> {
    let file = File::open(path)?;
    // 追記中の台帳でも、読み始めの範囲の確定行だけを使えば書き手を止めずに再生できる。
    let limit = file.metadata()?.len();
    let mut settings: Option<Settings> = None;
    let mut records: BTreeMap<String, Reading> = BTreeMap::new();
    for_each_complete_line(
        &mut BufReader::new(file.take(limit)),
        limit,
        |bytes| {
            let rec = parse_record(bytes)?;
            if rec.settings.fingerprint != fingerprint {
                return Ok(());
            }
            if rec.settings.computed_fingerprint()? != fingerprint {
                bail!("台帳の指紋と条件が合いません");
            }
            let selected = settings.get_or_insert_with(|| rec.settings.clone());
            if !selected.same_condition(&rec.settings) {
                bail!("同じ指紋の条件が一致しません");
            }
            merge_engine_version(selected, &rec.settings);
            rec.reading.validate(&rec.settings)?;
            if let Some(old) = records.get(&rec.reading.key)
                && !old.same_values(&rec.reading)
            {
                bail!("台帳の重複が矛盾します: {}", rec.reading.key);
            }
            records.insert(rec.reading.key.clone(), rec.reading);
            Ok(())
        },
        &mut false,
    )?;
    if settings.is_none() {
        bail!("指紋が台帳にありません");
    }
    Ok(records)
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

/// 最初の確定行が tree 台帳でなければ None、それ以外は確定行を走査して末尾の有無を返す。
pub(super) fn visit_records(
    path: &Path,
    mut visit: impl FnMut(Record) -> Result<()>,
) -> Result<Option<bool>> {
    let file = File::open(path).with_context(|| format!("台帳: {}", path.display()))?;
    let limit = file.metadata().with_context(|| format!("台帳: {}", path.display()))?.len();
    let mut reader = BufReader::new(file.take(limit));
    let mut first = Vec::new();
    let n = reader.read_until(b'\n', &mut first)?;
    if !first.ends_with(b"\n") {
        return Ok(None);
    }
    let is_tree = serde_json::from_slice::<serde_json::Value>(&first)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(|kind| kind.as_str())
                .map(|kind| kind.starts_with("book-tree-"))
        })
        .unwrap_or(false);
    if !is_tree {
        return Ok(None);
    }
    let mut ignored_tail = false;
    parse_record(&first)
        .and_then(&mut visit)
        .and_then(|()| {
            for_each_complete_line(
                &mut reader,
                limit - n as u64,
                |bytes| visit(parse_record(bytes)?),
                &mut ignored_tail,
            )
        })
        .with_context(|| format!("台帳: {}", path.display()))?;
    Ok(Some(ignored_tail))
}

pub(super) fn parse_record(bytes: &[u8]) -> Result<Record> {
    let rec: Record = serde_json::from_slice(bytes).context("tree 台帳の形式が不正です")?;
    if rec.kind != VERSION || rec.settings.version != VERSION {
        bail!("台帳の版が違います");
    }
    Ok(rec)
}

/// 読みを保持せず、同じ局面に異なる値が混ざっていないか確かめる。
fn remember_values(
    seen: &mut HashMap<[u8; 16], [u8; 16]>,
    reading: &Reading,
    fingerprint: &str,
) -> Result<bool> {
    let short = |bytes: &[u8]| -> [u8; 16] {
        Sha256::digest(bytes)[..16].try_into().expect("16 バイト")
    };
    let values = short(&serde_json::to_vec(&(&reading.multipv, &reading.bestmove))?);
    Ok(*seen
        .entry(short(&serde_json::to_vec(&(fingerprint, &reading.key))?))
        .or_insert(values)
        == values)
}

/// 全体を検査した取り込み元の、全条件の完全な行。
pub(super) struct StagedSource {
    pub file: File,
    pub sha256: String,
    pub row_marks: File,
    pub skipped_condition: usize,
    pub skipped_invalid: usize,
    pub ignored_tail: bool,
}

/// 全体を検査してから、全条件の行を入力順で一時ファイルに渡す。
pub(super) fn stage_live_source(path: &Path) -> Result<StagedSource> {
    super::paths::reject_symlink(path)?;
    if !std::fs::metadata(path)?.is_file() {
        bail!("取り込み元は通常のファイルが必要です");
    }
    let file = File::open(path)?;
    file_lock::try_lock_shared(&file).context("取り込み元の台帳が使用中です")?;
    let _unlock = UnlockOnDrop(file.try_clone()?);
    let mut reader = BufReader::new(file);
    let mut staged =
        tempfile::tempfile_in(path.parent().context("取り込み元の親 dir がありません")?)?;
    let mut hash = Sha256::new();
    let mut complete = 0u64;
    let mut ignored_tail = false;
    for_each_complete_line(
        &mut reader,
        u64::MAX,
        |bytes| {
            hash.update(bytes);
            complete += bytes.len() as u64;
            let _ = parse_record(bytes)?;
            Ok(())
        },
        &mut ignored_tail,
    )?;
    if complete == 0 {
        bail!("取り込み元の台帳が空です");
    }
    reader.seek(SeekFrom::Start(0))?;
    let dir = path.parent().context("取り込み元の親 dir がありません")?;
    let mut buckets = super::source_buckets::Buckets::new(dir, complete)?;
    let mut rows = 0;
    let mut validity = Validity::default();
    let mut skipped_rows = BTreeMap::<String, SkippedRows>::new();
    for_each_complete_line(
        &mut reader,
        complete,
        |bytes| {
            let rec = parse_record(bytes)?;
            if !validity.valid(&rec)? {
                skipped_rows.entry(rec.settings.fingerprint).or_default().invalid += 1;
                return Ok(());
            }
            buckets.push(&rec.reading, &rec.settings.fingerprint, rows, rec.prefetch)?;
            rows += 1;
            staged.write_all(bytes)?;
            Ok(())
        },
        &mut false,
    )?;
    let row_marks = buckets.finish(dir, rows)?;
    report_skipped(&skipped_rows);
    staged.seek(SeekFrom::Start(0))?;
    Ok(StagedSource {
        file: staged,
        sha256: format!("{:x}", hash.finalize()),
        row_marks,
        skipped_condition: 0,
        skipped_invalid: skipped_rows.values().map(|c| c.invalid).sum(),
        ignored_tail,
    })
}

#[derive(Default)]
struct Validity {
    fingerprints: BTreeMap<Vec<u8>, String>,
}

impl Validity {
    fn settings_valid(&mut self, settings: &Settings) -> Result<bool> {
        let normalized = serde_json::to_vec(&settings.normalized())?;
        let computed = self
            .fingerprints
            .entry(normalized)
            .or_insert_with_key(|bytes| format!("{:x}", Sha256::digest(bytes)));
        Ok(*computed == settings.fingerprint)
    }

    fn valid(&mut self, rec: &Record) -> Result<bool> {
        Ok(rec.reading.validate(&rec.settings).is_ok() && self.settings_valid(&rec.settings)?)
    }

    fn require(&mut self, reading: &Reading, settings: &Settings) -> Result<()> {
        reading.validate(settings)?;
        if !self.settings_valid(settings)? {
            bail!("台帳の指紋と条件が合いません");
        }
        Ok(())
    }
}

fn report_skipped(skipped: &BTreeMap<String, SkippedRows>) {
    for (fp, counts) in skipped {
        eprintln!(
            "journal skipped fingerprint={fp} invalid={} conflicting={}",
            counts.invalid, counts.conflicting
        );
    }
}

fn merge_engine_version(known: &mut Settings, other: &Settings) {
    if known.engine_version.is_none() {
        known.engine_version = other.engine_version.clone();
    }
}

/// 木に残る全ての読みを台帳と照合する。複数条件に一致する読みも確定扱いにしない。
pub(super) fn match_fingerprints(
    paths: &[PathBuf],
    trees: &[Vec<Reading>],
) -> Result<Vec<Option<String>>> {
    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Signature {
        key: String,
        nodes_limit: u64,
        nodes: Option<u64>,
        threads: usize,
        hash: u32,
        draw: BTreeMap<String, String>,
        multipv: Vec<(String, i32, u32)>,
        bestmove: String,
    }
    fn signature(r: &Reading) -> Signature {
        Signature {
            key: r.key.clone(),
            nodes_limit: r.nodes_limit,
            nodes: r.nodes,
            threads: r.threads,
            hash: r.hash,
            draw: r.draw.clone(),
            multipv: r.multipv.iter().map(|p| (p.mv.clone(), p.cp, p.depth)).collect(),
            bestmove: r.bestmove.clone(),
        }
    }
    let signatures: Vec<Vec<_>> =
        trees.iter().map(|rs| rs.iter().map(signature).collect()).collect();
    let mut wanted: BTreeMap<Signature, BTreeSet<String>> =
        signatures.iter().flatten().cloned().map(|s| (s, BTreeSet::new())).collect();
    if wanted.is_empty() {
        return Ok(vec![None; trees.len()]);
    }
    for path in paths.iter().collect::<BTreeSet<_>>() {
        let file =
            File::open(path).with_context(|| format!("台帳を読めません: {}", path.display()))?;
        // serve の追記を妨げず、読み始めの範囲だけで完結させる。
        let limit = file.metadata()?.len();
        let mut validity = Validity::default();
        let mut skipped = BTreeMap::<String, SkippedRows>::new();
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let dir = tempfile::Builder::new().prefix(".journal-match-").tempdir_in(parent)?;
        let mut buckets = super::source_buckets::Buckets::new(dir.path(), limit)?;
        let mut reader = BufReader::new(file.take(limit));
        let mut rows = 0;
        for_each_complete_line(
            &mut reader,
            limit,
            |bytes| {
                let rec = parse_record(bytes)?;
                if !validity.valid(&rec)? {
                    skipped.entry(rec.settings.fingerprint).or_default().invalid += 1;
                } else {
                    buckets.push(&rec.reading, &rec.settings.fingerprint, rows, false)?;
                    rows += 1;
                }
                Ok(())
            },
            &mut false,
        )?;
        let mut marks = BufReader::new(buckets.finish(dir.path(), rows)?);
        let mut file = reader.into_inner().into_inner();
        file.rewind()?;
        let mut reader = BufReader::new(file.take(limit));
        for_each_complete_line(
            &mut reader,
            limit,
            |bytes| {
                let rec = parse_record(bytes)?;
                if !validity.valid(&rec)? {
                    return Ok(());
                }
                let mut mark = [0];
                marks.read_exact(&mut mark)?;
                if mark[0] & 4 != 0 {
                    skipped.entry(rec.settings.fingerprint).or_default().conflicting += 1;
                } else if mark[0] & 2 == 0
                    && let Some(matches) = wanted.get_mut(&signature(&rec.reading))
                {
                    matches.insert(rec.settings.fingerprint);
                }
                Ok(())
            },
            &mut false,
        )?;
        report_skipped(&skipped);
    }
    Ok(signatures
        .iter()
        .map(|readings| {
            let mut fingerprint = None;
            for sig in readings {
                let matches = &wanted[sig];
                if matches.len() != 1 {
                    return None;
                }
                let found = matches.first().expect("1 件の指紋");
                if fingerprint.as_ref().is_some_and(|old| old != found) {
                    return None;
                }
                fingerprint = Some(found.clone());
            }
            fingerprint
        })
        .collect())
}

#[cfg(test)]
fn match_fingerprint(path: &Path, readings: &[Reading]) -> Result<Option<String>> {
    Ok(match_fingerprints(&[path.to_owned()], &[readings.to_vec()])?.remove(0))
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
            prefetch: false,
            reading,
        }
    }

    fn write_record(file: &mut File, record: &Record) {
        serde_json::to_writer(&mut *file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn staged_source_preserves_rows_and_aggregates_marks_without_retaining_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.jsonl");
        let mut first = record("", 0);
        first.settings.fingerprint = first.settings.computed_fingerprint().unwrap();
        let mut second = record(&first.settings.fingerprint, 8);
        second.prefetch = true;
        let mut duplicate = first.clone();
        duplicate.prefetch = true;
        duplicate.reading.ply = 99;
        let mut foreign = first.clone();
        foreign.settings.nodes += 1;
        foreign.reading.nodes_limit += 1;
        foreign.settings.fingerprint = foreign.settings.computed_fingerprint().unwrap();
        let mut file = File::create(&path).unwrap();
        for rec in [&first, &second, &duplicate, &foreign] {
            write_record(&mut file, rec);
        }
        file.write_all(b"unfinished").unwrap();
        drop(file);
        let mut expected = Vec::new();
        for rec in [&first, &second, &duplicate, &foreign] {
            serde_json::to_writer(&mut expected, rec).unwrap();
            expected.push(b'\n');
        }
        for _ in 0..2 {
            let mut source = stage_live_source(&path).unwrap();
            let mut rows = Vec::new();
            source.file.read_to_end(&mut rows).unwrap();
            assert_eq!(rows, expected);
            let mut marks = Vec::new();
            source.row_marks.read_to_end(&mut marks).unwrap();
            assert_eq!(marks, [1, 1, 2, 0]);
            assert_eq!(source.skipped_condition, 0);
            assert!(source.ignored_tail);
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn prefetch_metadata_and_recent_times_survive_load_and_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut file = File::create(&path).unwrap();
        for index in 1..=60 {
            rec.provenance = "engine".into();
            rec.prefetch = index == 60;
            rec.reading.sec = f64::from(index);
            write_record(&mut file, &rec);
        }
        rec.provenance = "journal:fixture".into();
        rec.reading.sec = 999.0;
        write_record(&mut file, &rec);
        drop(file);
        let journal = Journal::open(&path, &rec.settings.fingerprint).unwrap();
        assert_eq!(journal.prefetched, BTreeSet::from([rec.reading.key.clone()]));
        assert_eq!(
            journal.durations,
            (11..=60)
                .map(|n| f64::from(n) / rec.reading.nodes_limit as f64)
                .collect::<VecDeque<_>>()
        );
        drop(journal);
        let source = read_source(&path, None, |_| true).unwrap();
        let dst = dir.path().join("import.jsonl");
        let mut journal = Journal::open(&dst, &rec.settings.fingerprint).unwrap();
        let mut existing = rec.reading.clone();
        existing.ply = 99;
        journal.append(existing, &rec.settings, "journal:older".into()).unwrap();
        journal
            .merge_all(
                source.readings.into_iter().map(|r| (r, "journal:fixture".into())).collect(),
                &rec.settings,
                OnConflict::Reject,
                &mut NoConflictList,
                &source.prefetched.iter().map(|(_, key)| key.clone()).collect(),
            )
            .unwrap();
        drop(journal);
        let journal = Journal::open(&dst, &rec.settings.fingerprint).unwrap();
        assert!(journal.prefetched.contains(&rec.reading.key));
        assert_eq!(journal.get(&rec.reading.key).unwrap().ply, 99);
        assert!(journal.durations.is_empty());
        let mut old = serde_json::to_value(&rec).unwrap();
        old.as_object_mut().unwrap().remove("prefetch");
        assert!(!serde_json::from_value::<Record>(old).unwrap().prefetch);
    }

    #[test]
    fn matching_tree_readings_requires_one_condition_for_every_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut a = record("", 0);
        a.reading.multipv = vec![Pv {
            mv: "9i8i".into(),
            cp: 10,
            depth: 2,
        }];
        a.reading.bestmove = "9i8i".into();
        a.settings.fingerprint = a.settings.computed_fingerprint().unwrap();
        let mut b = record("", 8);
        b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
        let write = |records: &[Record]| {
            let mut file = File::create(&path).unwrap();
            for rec in records {
                write_record(&mut file, rec);
            }
        };
        let readings = vec![a.reading.clone(), b.reading.clone()];
        write(&[a.clone(), b.clone()]);
        assert_eq!(
            match_fingerprint(&path, &readings).unwrap(),
            Some(a.settings.fingerprint.clone())
        );
        let mut changed = readings.clone();
        changed[0].multipv[0].depth += 1;
        assert!(match_fingerprint(&path, &changed).unwrap().is_none());
        for change in 0..8 {
            let mut changed = readings.clone();
            match change {
                0 => changed[0].nodes_limit += 1,
                1 => changed[0].multipv[0].cp += 1,
                2 => changed[0].multipv[0].mv = "9i8h".into(),
                3 => changed[0].hash += 1,
                4 => {
                    changed[0].draw.insert("DrawValueBlack".into(), "70".into());
                }
                5 => changed[0].threads = 2,
                6 => changed[0].nodes = Some(101),
                _ => changed[0].bestmove = "resign".into(),
            }
            assert!(match_fingerprint(&path, &changed).unwrap().is_none());
        }
        write(std::slice::from_ref(&a));
        assert!(match_fingerprint(&path, &readings).unwrap().is_none());
        b.settings.engine_sha256 = "different".into();
        b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
        write(&[a.clone(), b.clone()]);
        assert!(match_fingerprint(&path, &readings).unwrap().is_none());
        let mut ambiguous = a.clone();
        ambiguous.settings = b.settings.clone();
        write(&[a.clone(), ambiguous]);
        assert!(match_fingerprint(&path, &[a.reading]).unwrap().is_none());
        let before = std::fs::read(&path).unwrap();
        assert!(match_fingerprint(&path, &[]).unwrap().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn simulation_reads_locked_journal_and_preserves_unfinished_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut journal = Journal::open(&path, &rec.settings.fingerprint).unwrap();
        journal.append(rec.reading.clone(), &rec.settings, "fixture".into()).unwrap();
        let mut other = record("", 1);
        other.settings.engine_sha256 = "other".into();
        other.settings.fingerprint = other.settings.computed_fingerprint().unwrap();
        write_record(&mut journal.lock.0, &other);
        journal.lock.0.write_all(b"{").unwrap();
        let before = std::fs::read(&path).unwrap();
        let readings = simulation_readings(&path, &rec.settings.fingerprint).unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(
            serde_json::to_value(&readings[&rec.reading.key]).unwrap(),
            serde_json::to_value(&rec.reading).unwrap()
        );
        assert!(simulation_readings(&path, "absent").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn matching_ignores_unfinished_tail_without_changing_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut file = File::create(&path).unwrap();
        write_record(&mut file, &rec);
        file.write_all(b"{\"reading\":").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            match_fingerprint(&path, &[rec.reading]).unwrap(),
            Some(rec.settings.fingerprint)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn matching_reads_exclusively_locked_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut journal = Journal::open(&path, &rec.settings.fingerprint).unwrap();
        journal.append(rec.reading.clone(), &rec.settings, "fixture".into()).unwrap();
        let probe = File::open(&path).unwrap();
        assert!(matches!(
            file_lock::try_lock_shared(&probe),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            match_fingerprint(&path, &[rec.reading]).unwrap(),
            Some(rec.settings.fingerprint)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(journal);
    }

    #[test]
    fn matching_rejects_invalid_complete_line_after_matching_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut file = File::create(&path).unwrap();
        write_record(&mut file, &rec);
        file.write_all(b"{invalid}\n{unfinished").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(match_fingerprint(&path, &[rec.reading]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn matching_combines_all_journals_for_all_trees() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("one"), dir.path().join("two")];
        let mut a = record("", 0);
        a.settings.fingerprint = a.settings.computed_fingerprint().unwrap();
        let mut b = record("", 8);
        b.settings.engine_sha256 = "different".into();
        b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
        let mut ambiguous = a.clone();
        ambiguous.settings = b.settings.clone();
        write_record(&mut File::create(&paths[0]).unwrap(), &a);
        let mut file = File::create(&paths[1]).unwrap();
        write_record(&mut file, &b);
        write_record(&mut file, &ambiguous);
        let trees = [
            vec![a.reading.clone()],
            vec![b.reading.clone()],
            vec![a.reading.clone(), b.reading.clone()],
            vec![],
        ];
        assert_eq!(
            match_fingerprints(&paths[..1], &trees).unwrap(),
            [Some(a.settings.fingerprint), None, None, None]
        );
        assert_eq!(
            match_fingerprints(&paths, &trees).unwrap(),
            [None, Some(b.settings.fingerprint), None, None]
        );
    }

    #[test]
    fn remember_does_not_time_zero_node_readings() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = record("", 0);
        rec.provenance = "engine".into();
        rec.reading.nodes_limit = 0;
        let mut journal = Journal::open(&dir.path().join("journal"), "").unwrap();
        for sec in [0.0, 0.1] {
            rec.reading.sec = sec;
            journal.remember(rec.clone());
        }
        assert!(journal.durations.is_empty());
        rec.reading.nodes_limit = 100;
        journal.remember(rec);
        assert_eq!(journal.durations, VecDeque::from([0.001]));
    }

    #[test]
    fn load_skips_tampered_fingerprints_and_accepts_equivalent_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut original = record("", 0);
        original.settings.options.insert("EvalFile".into(), "model-a".into());
        original.settings.content_sha256.insert("EvalFile".into(), "model-hash".into());
        original.settings.fingerprint = original.settings.computed_fingerprint().unwrap();
        let mut versioned = original.clone();
        versioned.reading = record("", 1).reading;
        versioned.settings.engine_version = Some("v1".into());
        let mut moved = versioned.clone();
        moved.reading = record("", 2).reading;
        moved.settings.options.insert("EvalFile".into(), "model-b".into());
        let mut conflicting = versioned.clone();
        conflicting.reading = record("", 3).reading;
        conflicting.settings.engine_version = Some("v2".into());
        let mut tampered = original.clone();
        tampered.reading = record("", 4).reading;
        tampered.settings.engine_sha256 = "tampered".into();
        let mut file = File::create(&path).unwrap();
        for rec in [
            &original,
            &versioned,
            &moved,
            &conflicting,
            &tampered,
            &moved,
            &original,
        ] {
            write_record(&mut file, rec);
        }
        drop(file);
        let journal = Journal::open(&path, &original.settings.fingerprint).unwrap();
        for rec in [&original, &versioned, &moved, &conflicting] {
            assert_eq!(journal.get(&rec.reading.key), Some(&rec.reading));
        }
        assert!(!journal.has_exact(&tampered.reading.key, &original.settings.fingerprint));
        assert_eq!(journal.skipped_rows[&original.settings.fingerprint].invalid, 1);
        assert_eq!(journal.records.len(), 4);
    }

    #[test]
    fn load_accepts_engine_version_added_after_first_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut first = record("", 0);
        first.settings.fingerprint = first.settings.computed_fingerprint().unwrap();
        let mut file = File::create(&path).unwrap();
        write_record(&mut file, &first);
        for column in 1..4 {
            let mut later = record(&first.settings.fingerprint, column);
            later.settings.engine_version = Some("v1".into());
            write_record(&mut file, &later);
        }
        drop(file);
        let journal = Journal::open(&path, &first.settings.fingerprint).unwrap();
        assert_eq!(journal.records.len(), 4);
        assert!(journal.skipped_rows.is_empty());
        assert_eq!(journal.conditions[0].engine_version.as_deref(), Some("v1"));
    }

    #[test]
    fn load_keeps_valid_rows_after_corrupt_first_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut valid = record("", 0);
        valid.settings.fingerprint = valid.settings.computed_fingerprint().unwrap();
        let mut corrupt = valid.clone();
        corrupt.settings.engine_sha256 = "tampered".into();
        let mut later = record(&valid.settings.fingerprint, 1);
        later.settings.engine_version = Some("v1".into());
        let mut file = File::create(&path).unwrap();
        for rec in [&corrupt, &valid, &later] {
            write_record(&mut file, rec);
        }
        drop(file);
        let journal = Journal::open(&path, &valid.settings.fingerprint).unwrap();
        assert_eq!(journal.records.len(), 2);
        assert_eq!(journal.get(&valid.reading.key), Some(&valid.reading));
        assert_eq!(journal.get(&later.reading.key), Some(&later.reading));
        assert_eq!(journal.skipped_rows[&valid.settings.fingerprint].invalid, 1);
    }

    #[test]
    fn journal_retains_all_conditions_and_prefers_requested_condition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut a = record("first", 0);
        a.settings.fingerprint = a.settings.computed_fingerprint().unwrap();
        let mut b = record("second", 0);
        b.reading.bestmove = "win".into();
        b.settings.engine_sha256 = "other".into();
        b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
        let fallback = if a.settings.fingerprint < b.settings.fingerprint {
            &a.reading
        } else {
            &b.reading
        };
        let mut journal = Journal::open(&path, &a.settings.fingerprint).unwrap();
        journal.append(a.reading.clone(), &a.settings, "fixture".into()).unwrap();
        journal.append(b.reading.clone(), &b.settings, "fixture".into()).unwrap();
        assert_eq!(journal.get(&a.reading.key), Some(&a.reading));
        assert_eq!(
            journal.get_preferred(&a.reading.key, Some(&b.settings.fingerprint)),
            Some(&b.reading)
        );
        assert_eq!(journal.get_preferred(&a.reading.key, Some("absent")), Some(fallback));
        assert!(journal.has_exact(&a.reading.key, &a.settings.fingerprint));
        assert!(!journal.has_exact(&a.reading.key, "absent"));
        drop(journal);
        let journal = Journal::open(&path, "absent").unwrap();
        assert_eq!(journal.get(&a.reading.key), Some(fallback));
        assert_eq!(journal.records[&a.reading.key].as_slice().len(), 2);
        drop(journal);
        let mut bad = a.clone();
        bad.reading.bestmove = "win".into();
        write_record(&mut OpenOptions::new().append(true).open(&path).unwrap(), &bad);
        let mut invalid = record("old-condition", 0);
        invalid.reading.threads = 2;
        invalid.settings.fingerprint = invalid.settings.computed_fingerprint().unwrap();
        write_record(&mut OpenOptions::new().append(true).open(&path).unwrap(), &invalid);
        let bytes = std::fs::read(&path).unwrap();
        let mut journal = Journal::open(&path, &b.settings.fingerprint).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(journal.get(&b.reading.key), Some(&b.reading));
        assert_eq!(journal.skipped_rows[&invalid.settings.fingerprint].invalid, 1);
        assert_eq!(journal.skipped_rows[&a.settings.fingerprint].conflicting, 1);
        assert_eq!(journal.get_exact(&a.reading.key, &a.settings.fingerprint), Some(&a.reading));
        assert!(journal.append(bad.reading, &bad.settings, "engine".into()).is_err());
    }

    #[test]
    fn duplicate_append_is_idempotent_and_keeps_inline_single_row() {
        for mark in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("journal");
            let mut a = record("a", 0);
            a.settings.fingerprint = a.settings.computed_fingerprint().unwrap();
            let mut b = record("b", 0);
            b.reading.bestmove = "win".into();
            b.settings.engine_sha256 = "other".into();
            b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
            let mut journal = Journal::open(&path, &a.settings.fingerprint).unwrap();
            journal.append(a.reading.clone(), &a.settings, "fixture".into()).unwrap();
            assert!(matches!(journal.records[&a.reading.key], Rows::One(_)));
            journal.append(b.reading.clone(), &b.settings, "fixture".into()).unwrap();
            let size = std::fs::metadata(&path).unwrap().len();
            journal
                .append_marked(a.reading.clone(), &a.settings, "fixture".into(), mark)
                .unwrap();
            let after = std::fs::metadata(&path).unwrap().len();
            assert_eq!(after > size, mark);
            journal
                .append_marked(a.reading.clone(), &a.settings, "fixture".into(), mark)
                .unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().len(), after);
            assert_eq!(journal.get(&a.reading.key), Some(&a.reading));
            drop(journal);
            let journal = Journal::open(&path, &a.settings.fingerprint).unwrap();
            assert_eq!(journal.get(&a.reading.key), Some(&a.reading));
            assert_eq!(journal.records[&a.reading.key].as_slice().len(), 2);
        }
    }

    #[test]
    fn fallback_is_independent_of_order_and_ranks_nodes_pvs_then_fingerprint() {
        let mut a = record("a", 0);
        a.settings.fingerprint = a.settings.computed_fingerprint().unwrap();
        let mut b = record("b", 0);
        b.reading.bestmove = "win".into();
        b.settings.engine_sha256 = "other".into();
        b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
        for (nodes, extra_pv) in [(100, false), (101, false), (100, true)] {
            let mut b = b.clone();
            b.settings.nodes = nodes;
            b.reading.nodes_limit = nodes;
            if extra_pv {
                b.reading.bestmove = "9i8i".into();
                b.reading.multipv.push(Pv {
                    mv: "9i8i".into(),
                    cp: 10,
                    depth: 1,
                });
            }
            b.settings.fingerprint = b.settings.computed_fingerprint().unwrap();
            let expected = if nodes > 100 || extra_pv {
                b.settings.fingerprint.as_str()
            } else {
                std::cmp::min(a.settings.fingerprint.as_str(), b.settings.fingerprint.as_str())
            };
            for reverse in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("journal");
                let mut file = File::create(&path).unwrap();
                for rec in if reverse { [&b, &a] } else { [&a, &b] } {
                    write_record(&mut file, rec);
                }
                drop(file);
                let journal = Journal::open(&path, "absent").unwrap();
                assert_eq!(journal.condition(&a.reading.key, None), Some(expected));
                assert_eq!(
                    journal.condition(&a.reading.key, Some(&a.settings.fingerprint)),
                    Some(a.settings.fingerprint.as_str())
                );
            }
        }
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
        assert!(journal.skipped_rows.is_empty());
    }

    #[test]
    fn streaming_select_lists_settings_and_preserves_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut first = record("first", 0);
        first.settings.fingerprint = first.settings.computed_fingerprint().unwrap();
        let mut second = record("second", 1);
        second.settings.engine_sha256 = "other-engine".into();
        second.settings.fingerprint = second.settings.computed_fingerprint().unwrap();
        {
            let mut file = File::create(&path).unwrap();
            for rec in [&first, &second, &first] {
                write_record(&mut file, rec);
            }
        }
        let lock = LockedFile::open(&path).unwrap();
        let (all, _) = Journal::select(lock, None).unwrap();
        assert_eq!(all.conditions.len(), 2);
        assert_eq!(all.records.len(), 2);
        drop(all);
        assert!(Journal::select(LockedFile::open(&path).unwrap(), Some("absent")).is_err());
        let (journal, settings) =
            Journal::select(LockedFile::open(&path).unwrap(), Some(&second.settings.fingerprint))
                .unwrap();
        assert_eq!(settings, Some(second.settings.clone()));
        assert_eq!(journal.records.len(), 2);
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
        assert_eq!(settings, None);
        assert_eq!(journal.records.len(), 1);
        drop(journal);
        File::create(&path).unwrap();
        assert!(Journal::select(LockedFile::open(&path).unwrap(), None).is_err());
    }

    #[test]
    fn all_conditions_validate_readings_and_version() {
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
        invalid_reading.settings.fingerprint =
            invalid_reading.settings.computed_fingerprint().unwrap();
        {
            let mut file = File::create(&path).unwrap();
            write_record(&mut file, &invalid_reading);
        }
        let journal = Journal::open(&path, "target").unwrap();
        assert_eq!(journal.skipped_rows[&invalid_reading.settings.fingerprint].invalid, 1);
        assert!(journal.get(&invalid_reading.reading.key).is_none());
        drop(journal);
        assert!(Journal::open(&path, "past").is_ok());
        assert!(Journal::select(LockedFile::open(&path).unwrap(), None).is_ok());
        std::fs::write(&path, b"{invalid}\n").unwrap();
        assert!(Journal::open(&path, "target").is_err());
    }
    #[test]
    fn skipped_rows_do_not_register_or_update_conditions_or_prefetch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut valid = record("", 0);
        valid.settings.fingerprint = valid.settings.computed_fingerprint().unwrap();
        valid.settings.engine_version = Some("kept".into());
        let mut invalid = valid.clone();
        invalid.settings.engine_version = Some("invalid".into());
        invalid.reading.threads = 2;
        invalid.prefetch = true;
        let mut conflicting = valid.clone();
        conflicting.reading.bestmove = "win".into();
        conflicting.settings.engine_version = Some("conflicting".into());
        conflicting.prefetch = true;
        let mut file = File::create(&path).unwrap();
        write_record(&mut file, &invalid);
        drop(file);
        let journal = Journal::open(&path, &valid.settings.fingerprint).unwrap();
        assert!(journal.conditions.is_empty());
        assert!(journal.prefetched.is_empty());
        drop(journal);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        write_record(&mut file, &valid);
        write_record(&mut file, &conflicting);
        drop(file);
        let journal = Journal::open(&path, &valid.settings.fingerprint).unwrap();
        assert_eq!(journal.conditions.len(), 1);
        assert_eq!(journal.conditions[0].engine_version.as_deref(), Some("kept"));
        assert!(journal.prefetched.is_empty());
        assert_eq!(journal.skipped_rows[&valid.settings.fingerprint].invalid, 1);
        assert_eq!(journal.skipped_rows[&valid.settings.fingerprint].conflicting, 1);
        drop(journal);
        let source = read_source(&path, None, |_| true).unwrap();
        assert_eq!(source.readings, [valid.reading.clone()]);
        assert_eq!(source.reading_settings[0].engine_version.as_deref(), Some("kept"));
        assert!(source.prefetched.is_empty());
        assert_eq!(match_fingerprint(&path, &[conflicting.reading]).unwrap(), None);
        assert_eq!(
            match_fingerprint(&path, &[valid.reading]).unwrap(),
            Some(valid.settings.fingerprint)
        );
    }

    #[test]
    fn load_caches_fingerprint_per_normalized_settings() {
        let mut rec = record("", 0);
        rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
        let mut validity = Validity::default();
        assert!(validity.valid(&rec).unwrap());
        rec.settings.engine_version = Some("other".into());
        assert!(validity.valid(&rec).unwrap());
        rec.settings.fingerprint = "forged".into();
        assert!(!validity.valid(&rec).unwrap());
        assert_eq!(validity.fingerprints.len(), 1);
    }

    #[test]
    fn duration_samples_are_normalized_across_node_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut journal = Journal::open(&path, "").unwrap();
        for (column, nodes, sec) in [(0, 100, 2.0), (1, 1000, 20.0)] {
            let mut rec = record("", column);
            rec.settings.nodes = nodes;
            rec.settings.fingerprint = rec.settings.computed_fingerprint().unwrap();
            rec.reading.nodes_limit = nodes;
            rec.reading.sec = sec;
            journal.append(rec.reading, &rec.settings, "engine".into()).unwrap();
        }
        assert_eq!(journal.durations, [0.02, 0.02]);
        drop(journal);
        assert_eq!(Journal::open(&path, "").unwrap().durations, [0.02, 0.02]);
    }
}
