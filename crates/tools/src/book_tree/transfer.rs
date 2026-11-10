//! 別の台帳の読みを、読みごとの条件を保って取り込む。

use super::ReadingArgs;
use super::journal::{
    ConflictSink, Journal, NoConflictList, OnConflict, Reading, Settings, offset_signature,
    read_source_incremental, source_identity,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Args, Debug)]
pub(super) struct ImportJournalArgs {
    /// 取り込み元の台帳。書き込み中の台帳は拒否する (複製してから渡す)
    #[arg(long, required = true)]
    from_journal: Vec<PathBuf>,
    /// ソースごとの再開位置を保存する JSON
    #[arg(long)]
    offsets: Option<PathBuf>,
    /// 取り込まない読みの JSONL
    #[arg(long)]
    exclude_readings: Option<PathBuf>,
    /// 取り込み元の条件の指紋。省略時は全条件
    #[arg(long)]
    fingerprint: Option<String>,
    /// 互換性のために受理する旧フラグ (効果なし)
    #[arg(long, value_name = "FP", value_parser = super::parse_accepted_fingerprint)]
    accept_fingerprint: Vec<String>,
    /// 取り込み先の台帳
    #[arg(long)]
    journal: PathBuf,
    /// 表示用 fingerprint の算出条件 (取り込みの可否には使わない)
    #[command(flatten)]
    reading: ReadingArgs,
    /// 互換性のために受理する旧フラグ (効果なし)
    #[arg(long)]
    force: bool,
    /// 取り込み先と値の違う重複の扱い。keep-existing は取り込み先の値を残す (取り込み元内の食い違いは飛ばす)
    #[arg(long, value_enum, default_value_t = OnConflict::Reject)]
    on_conflict: OnConflict,
    /// keep-existing で残さなかった読みの一覧 (TSV) の書き出し先。keep-existing でだけ使い、必須
    #[arg(long)]
    conflicts_out: Option<PathBuf>,
}

/// `--source-version` に答えるエンジンならソースの版を返す。答えなければ `None`。
pub(super) fn engine_source_version(engine: &Path) -> Option<String> {
    query_source_version(engine, VERSION_TIMEOUT)
}

fn query_source_version(engine: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new(engine)
        .arg("--source-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // 起動したプロセスの子が stdout を持ったまま残ると読み終わらないので、読むのも期限付きにする。
    std::thread::spawn(move || {
        let mut text = String::new();
        let read = stdout.read_to_string(&mut text).map(|_| text);
        let _ = tx.send(read);
    });
    let deadline = Instant::now() + timeout;
    let text = rx.recv_timeout(timeout).ok().and_then(Result::ok);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if text.is_some() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => break None,
        }
    };
    if status.is_none() {
        // 引数を USI のコマンドとして待ち続けるエンジンもある。
        let _ = child.kill();
        let _ = child.wait();
    }
    parse_source_version(&text?).filter(|_| status.is_some_and(|s| s.success()))
}

/// 版が分からないときに stderr へ 1 行の警告を出す。
pub(super) fn warn_unknown_version(engine: &Path, version: &Option<String>) {
    if let Some(warning) = version_warning(version.as_deref()) {
        eprintln!("警告: {warning}: {}", engine.display());
    }
}

/// 別のバイナリとの間で読みを取り込むのに使えない版なら、その理由を返す。
fn version_warning(version: Option<&str>) -> Option<&'static str> {
    match version {
        None => Some(
            "エンジンが --source-version に答えないので版が分かりません。読みには記録済みの条件を使います",
        ),
        Some(v) if v.starts_with("unknown") => Some(
            "エンジンの版が unknown です (git の作業木の外でのビルド、または git が使えない)。読みには記録済みの条件を使います",
        ),
        Some(v) if v.contains("-dirty") => Some(
            "エンジンは commit されていない変更を含むビルド (-dirty) です。読みには記録済みの条件を使います",
        ),
        Some(_) => None,
    }
}

fn parse_source_version(stdout: &str) -> Option<String> {
    let version = stdout.lines().find_map(|line| line.trim().strip_prefix("source-version "))?;
    let version = version.trim();
    (!version.is_empty() && !version.contains(char::is_whitespace)).then(|| version.to_owned())
}

pub(super) fn run(args: ImportJournalArgs) -> Result<()> {
    import(args).map(|_| ())
}

fn import(args: ImportJournalArgs) -> Result<(usize, usize, usize)> {
    match (args.on_conflict, &args.conflicts_out) {
        (OnConflict::KeepExisting, None) => {
            bail!("--on-conflict keep-existing には --conflicts-out が要ります")
        }
        (OnConflict::Reject, Some(_)) => {
            bail!("--conflicts-out は --on-conflict keep-existing でだけ使えます")
        }
        _ => {}
    }
    let mut paths = BTreeSet::new();
    for source in &args.from_journal {
        let canonical = std::fs::canonicalize(source)
            .with_context(|| format!("取り込み元: {}", source.display()))?;
        if !paths.insert(canonical) {
            bail!("取り込み元のパスが重複しています: {}", source.display());
        }
        if same_path(&args.journal, source)? {
            bail!("取り込み元と取り込み先が同じ台帳です");
        }
    }
    // 状態や一覧の置換が、入力や台帳を上書きしないようにする。
    let protected: Vec<_> = args
        .from_journal
        .iter()
        .chain(std::iter::once(&args.journal))
        .chain(args.exclude_readings.iter())
        .collect();
    for out in args.conflicts_out.iter().chain(args.offsets.iter()) {
        for path in &protected {
            if same_path(out, path)? {
                bail!(
                    "出力先 ({}) が台帳または除外リスト ({}) と同じです",
                    out.display(),
                    path.display()
                );
            }
        }
    }
    if let (Some(out), Some(offsets)) = (&args.conflicts_out, &args.offsets)
        && same_path(out, offsets)?
    {
        bail!("--offsets と --conflicts-out が同じです");
    }
    let (excluded, excluded_sha256) = load_excluded(args.exclude_readings.as_deref())?;
    let mut state = None;
    let mut dst = Settings::new(&args.reading)?;
    dst.engine_version = engine_source_version(&args.reading.engine);
    warn_unknown_version(&args.reading.engine, &dst.engine_version);
    let mut list = args
        .conflicts_out
        .as_ref()
        .map(|path| {
            ConflictTsv::create(path)
                .with_context(|| format!("食い違いの一覧を {} に作れません", path.display()))
        })
        .transpose()?;
    let mut journal = Journal::open_checked(&args.journal, &dst.fingerprint, |file| {
        state = match &args.offsets {
            Some(path) if path.try_exists()? => Some(
                serde_json::from_reader::<_, OffsetState>(BufReader::new(std::fs::File::open(path)?))
                    .with_context(|| format!("再開状態を読めません: {}。形式が異なる場合は新しい取り込み先を用意し、offsets ファイルを削除してください", path.display()))?,
            ),
            _ => None,
        };
        if state.as_ref().is_some_and(|s| s.excluded_sha256 != excluded_sha256) {
            bail!(
                "除外リストが再開状態と一致しません。新しい取り込み先を用意し、offsets ファイルを削除してください"
            );
        }
        if let Some(saved) = &state
            && (!args.journal.try_exists()?
                || std::fs::canonicalize(&args.journal)? != saved.destination.path)
        {
            bail!(
                "取り込み先が再開状態と一致しません。新しい取り込み先を用意し、offsets ファイルを削除してください"
            );
        }
        if state.as_ref().is_some_and(|s| s.fingerprint != args.fingerprint) {
            bail!(
                "指紋フィルタが再開状態と一致しません。新しい取り込み先を用意し、offsets ファイルを削除してください"
            );
        }
        if let Some(saved) = &state {
            let identity = &saved.destination;
            if file.metadata()?.len() < identity.offset
                || offset_signature(file, identity.offset)? != identity.sig
            {
                bail!(
                    "取り込み先の内容が再開状態と一致しません。新しい取り込み先を用意し、offsets ファイルを削除してください"
                );
            }
        }
        Ok(())
    })?;
    let destination_path = std::fs::canonicalize(&args.journal)?;
    let state = state.get_or_insert_with(|| OffsetState {
        destination: DestinationState {
            path: destination_path.clone(),
            offset: 0,
            sig: String::new(),
        },
        excluded_sha256,
        fingerprint: args.fingerprint.clone(),
        sources: BTreeMap::new(),
    });
    let mut offsets_tmp = args
        .offsets
        .as_ref()
        .map(|path| tempfile::NamedTempFile::new_in(output_dir(path)))
        .transpose()?;
    if args.from_journal.len() > 1 {
        journal.stage_appends(output_dir(&args.journal))?;
    }
    let mut totals = (0, 0, 0);
    let mut fingerprints = BTreeSet::new();
    let mut reports = Vec::new();
    for path in &args.from_journal {
        let canonical = std::fs::canonicalize(path)?;
        let resume = args
            .offsets
            .as_ref()
            .and_then(|_| state.sources.get(&canonical))
            .map(|entry| (entry.offset, entry.sig.as_str()));
        let mut source = read_source_incremental(
            path,
            args.fingerprint.as_deref(),
            |_| true,
            resume,
            |reading| excluded.binary_search(&ExcludedReading::identity(reading)).is_ok(),
        )?;
        let mut destination_conflicts = if source.start == 0 {
            BTreeSet::new()
        } else {
            state.sources[&canonical].destination_conflicts.clone()
        };
        source.skip_prefix_conflicts(path, &journal, &destination_conflicts, |reading| {
            excluded.binary_search(&ExcludedReading::identity(reading)).is_ok()
        })?;
        if source.ignored_tail {
            eprintln!("取り込み元の末尾の改行で終わらない行は無視しました: {}", path.display());
        }
        fingerprints.extend(source.reading_settings.iter().map(|s| s.fingerprint.clone()));
        if args.offsets.is_some() && args.on_conflict == OnConflict::KeepExisting {
            for (reading, settings) in source.readings.iter().zip(&source.reading_settings) {
                if journal
                    .get_exact(&reading.key, &settings.fingerprint)
                    .is_some_and(|old| !old.same_values(reading))
                {
                    destination_conflicts
                        .insert(source_identity(&settings.fingerprint, &reading.key)?);
                }
            }
        }
        let incoming = source
            .readings
            .into_iter()
            .zip(source.reading_settings)
            .map(|(reading, settings)| {
                let mut provenance = format!(
                    "journal:sha256={}:fingerprint={}:force=false:path={}",
                    source.sha256,
                    settings.fingerprint,
                    path.display()
                );
                if source.start > 0 {
                    provenance.push_str(&format!(":offset={}", source.start));
                }
                (reading, settings, provenance)
            })
            .collect();
        let mut deferred = DeferredConflict(list.as_mut(), args.from_journal.len() > 1);
        let counts =
            journal.merge_records(incoming, args.on_conflict, &mut deferred, &source.prefetched)?;
        totals.0 += counts.0;
        totals.1 += counts.1;
        totals.2 += counts.2;
        reports.push(source_report(
            path,
            source.start..source.end,
            counts,
            source.excluded,
            source.rewritten,
        ));
        if args.offsets.is_some() {
            state.sources.insert(
                canonical,
                SourceOffset {
                    offset: source.end,
                    sig: source.sig,
                    destination_conflicts,
                },
            );
        }
    }
    if args.from_journal.len() > 1
        && let Some(list) = &mut list
    {
        list.before_append()?;
    }
    journal.commit_appends()?;
    if let Some(path) = &args.offsets {
        let (offset, sig) = journal.identity()?;
        state.destination = DestinationState {
            path: destination_path,
            offset,
            sig,
        };
        write_offsets(
            path,
            state,
            offsets_tmp.take().context("再開状態の一時ファイルがありません")?,
        )?;
    }
    for report in reports {
        println!("{report}");
    }
    println!("fingerprint={}", dst.fingerprint);
    println!("source_fingerprint={}", fingerprints.into_iter().collect::<Vec<_>>().join(","));
    println!("appended={} skipped={} conflicts={}", totals.0, totals.1, totals.2);
    Ok(totals)
}

fn source_report(
    path: &Path,
    range: std::ops::Range<u64>,
    counts: (usize, usize, usize),
    excluded: usize,
    rewritten: bool,
) -> String {
    format!(
        "source={} start={} end={} appended={} skipped={} conflicts={} excluded={}{}",
        path.display(),
        range.start,
        range.end,
        counts.0,
        counts.1,
        counts.2,
        excluded,
        if rewritten { " rewritten=1" } else { "" }
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceOffset {
    offset: u64,
    sig: String,
    destination_conflicts: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationState {
    path: PathBuf,
    offset: u64,
    sig: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OffsetState {
    /// null を許容しつつ、フィールドの省略は拒否する。
    #[serde(deserialize_with = "Deserialize::deserialize")]
    fingerprint: Option<String>,
    destination: DestinationState,
    excluded_sha256: Option<String>,
    sources: BTreeMap<PathBuf, SourceOffset>,
}

#[derive(Deserialize)]
struct ExcludedReading {
    key: String,
    ply: u32,
    nodes: u64,
    sec: f64,
}

type ExcludedIdentity = [u8; 16];

impl ExcludedReading {
    fn identity(reading: &Reading) -> ExcludedIdentity {
        Self::digest(&reading.key, reading.ply, reading.nodes, reading.sec)
    }

    fn digest(key: &str, ply: u32, nodes: Option<u64>, sec: f64) -> ExcludedIdentity {
        let mut hash = Sha256::new();
        hash.update(key.as_bytes());
        hash.update(ply.to_le_bytes());
        hash.update([u8::from(nodes.is_some())]);
        hash.update(nodes.unwrap_or_default().to_le_bytes());
        hash.update(sec_bits(sec).to_le_bytes());
        hash.finalize()[..16].try_into().expect("16 バイト")
    }
}

fn sec_bits(sec: f64) -> u64 {
    if sec == 0.0 { 0 } else { sec.to_bits() }
}

fn load_excluded(path: Option<&Path>) -> Result<(Vec<ExcludedIdentity>, Option<String>)> {
    let Some(path) = path else {
        return Ok((Vec::new(), None));
    };
    // 除外対象は既にメモリにある台帳索引の読みの部分集合なので、約 16 バイト × 件数を保持する。
    let mut excluded = Vec::new();
    let mut hash = Sha256::new();
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut bytes = Vec::new();
    let mut line = 0;
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        line += 1;
        hash.update(&bytes);
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let row: ExcludedReading = serde_json::from_slice(&bytes)
            .with_context(|| format!("除外リスト {} の {line} 行目が不正です", path.display()))?;
        excluded.push(ExcludedReading::digest(&row.key, row.ply, Some(row.nodes), row.sec));
    }
    excluded.sort_unstable();
    excluded.dedup();
    Ok((excluded, Some(format!("{:x}", hash.finalize()))))
}

fn output_dir(path: &Path) -> &Path {
    path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn write_offsets(path: &Path, state: &OffsetState, mut tmp: tempfile::NamedTempFile) -> Result<()> {
    {
        let mut writer = BufWriter::new(&mut tmp);
        serde_json::to_writer(&mut writer, state)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    crate::common::io::sync_directory(output_dir(path))
}

struct DeferredConflict<'a>(Option<&'a mut ConflictTsv>, bool);

impl ConflictSink for DeferredConflict<'_> {
    fn conflict(&mut self, existing: &Reading, incoming: &Reading, provenance: &str) -> Result<()> {
        match &mut self.0 {
            Some(list) => list.conflict(existing, incoming, provenance),
            None => NoConflictList.conflict(existing, incoming, provenance),
        }
    }

    fn before_append(&mut self) -> Result<()> {
        match &mut self.0 {
            Some(list) if self.1 => list.checkpoint(),
            Some(list) => list.before_append(),
            None => Ok(()),
        }
    }
}

const CONFLICT_HEADER: &str = "key\tply\texisting_bestmove\texisting_cp\tincoming_bestmove\tincoming_cp\tincoming_provenance\n";

fn first_cp(reading: &super::journal::Reading) -> String {
    reading.lines().first().map(|pv| pv.cp.to_string()).unwrap_or_default()
}

/// 取り込み元のパスがタブ・改行を含んでも列と行が崩れないよう、`\` `\t` `\n` `\r` で書く。
fn escape_tsv(field: &str) -> String {
    field
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// 存在するファイルは実体で、存在しないファイルは一番近い存在する祖先の実体に残りの部分を足して比べる
/// (取り込み先の台帳の dir はまだ無くてよい)。
fn same_path(a: &Path, b: &Path) -> Result<bool> {
    let resolve = |p: &Path| -> Result<PathBuf> {
        let mut base = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()?.join(p)
        };
        let mut rest = Vec::new();
        while !base.try_exists()? {
            rest.push(base.file_name().context("パスを解決できません")?.to_owned());
            base = base.parent().context("パスを解決できません")?.to_path_buf();
        }
        let mut resolved = std::fs::canonicalize(&base)?;
        resolved.extend(rest.iter().rev());
        Ok(resolved)
    };
    if a.try_exists()? && b.try_exists()? && same_file::is_same_file(a, b)? {
        return Ok(true);
    }
    Ok(resolve(a)? == resolve(b)?)
}

/// 食い違いの一覧。同じ dir の一時ファイルに 1 行ずつ書き、`before_append` で fsync してから置き換え、dir も同期する
/// (書きかけの一覧を残さない)。置き換えなかったときは、自分が作った一時ファイルだけが消える。
/// 食い違いが 0 件でもヘッダだけの一覧を書く。
pub(super) struct ConflictTsv {
    path: PathBuf,
    dir: PathBuf,
    tmp: Option<std::io::BufWriter<tempfile::NamedTempFile>>,
}

impl ConflictTsv {
    /// 公開前の一覧を同期する。複数周回に分けて追記する取り込みで使う。
    pub(super) fn checkpoint(&mut self) -> Result<()> {
        use std::io::Write;
        let tmp = self.tmp.as_mut().context("食い違いの一覧はもう置き換えました")?;
        tmp.flush()?;
        tmp.get_ref().as_file().sync_data()?;
        Ok(())
    }

    pub(super) fn create(path: &Path) -> Result<Self> {
        use std::io::Write;
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let mut tmp = std::io::BufWriter::new(tempfile::NamedTempFile::new_in(&dir)?);
        tmp.write_all(CONFLICT_HEADER.as_bytes())?;
        Ok(Self {
            path: path.to_path_buf(),
            dir,
            tmp: Some(tmp),
        })
    }
}

impl ConflictSink for ConflictTsv {
    fn conflict(&mut self, existing: &Reading, incoming: &Reading, provenance: &str) -> Result<()> {
        use std::io::Write;
        let fields = [
            escape_tsv(&incoming.key),
            incoming.ply.to_string(),
            escape_tsv(&existing.bestmove),
            first_cp(existing),
            escape_tsv(&incoming.bestmove),
            first_cp(incoming),
            escape_tsv(provenance),
        ];
        let tmp = self.tmp.as_mut().context("食い違いの一覧はもう置き換えました")?;
        writeln!(tmp, "{}", fields.join("\t"))?;
        Ok(())
    }

    fn before_append(&mut self) -> Result<()> {
        let tmp = self.tmp.take().context("食い違いの一覧はもう置き換えました")?;
        let tmp = tmp.into_inner().map_err(|e| e.into_error())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        crate::common::io::sync_directory(&self.dir)
            .with_context(|| format!("{} を置き換えた dir を同期できません", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_tree::journal::{LockedFile, Reading, read_source, read_source_incremental};
    use std::collections::{BTreeMap, BTreeSet};

    const VERSION_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const VERSION_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    struct Machine {
        engine: PathBuf,
        model: PathBuf,
    }

    fn machine(dir: &Path, name: &str, engine: &str, model: &str) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let m = Machine {
            engine: root.join("engine"),
            model: root.join("model.bin"),
        };
        std::fs::write(&m.engine, engine).unwrap();
        std::fs::write(&m.model, model).unwrap();
        m
    }

    fn reading_args(m: &Machine, nodes: u64, extra: &[&str]) -> ReadingArgs {
        let mut engine_options = vec![format!("EvalFile={}", m.model.display())];
        engine_options.extend(extra.iter().map(|s| s.to_string()));
        ReadingArgs {
            engine: m.engine.clone(),
            engine_options,
            nodes,
            multipv: 1,
            hash: 16,
        }
    }

    fn settings(m: &Machine, version: Option<&str>) -> Settings {
        let mut s = Settings::new(&reading_args(m, 100, &["DrawValueBlack=-50"])).unwrap();
        s.engine_version = version.map(str::to_owned);
        s
    }

    fn reading(column: usize, settings: &Settings, bestmove: &str) -> Reading {
        let row = match column {
            0 => "K8".into(),
            8 => "8K".into(),
            n => format!("{n}K{}", 8 - n),
        };
        Reading {
            tag: "fixture".into(),
            ply: 1,
            key: format!("4k4/9/9/9/9/9/9/9/{row} b -"),
            nodes_limit: settings.nodes,
            nodes: Some(100),
            sec: 0.1,
            threads: 1,
            hash: settings.hash,
            draw: settings.draw(),
            multipv: Vec::new(),
            bestmove: bestmove.into(),
        }
    }

    fn write_journal(path: &Path, rows: &[(&Settings, Reading)]) {
        for (settings, reading) in rows {
            let mut journal = Journal::open(path, &settings.fingerprint).unwrap();
            journal.append(reading.clone(), settings, "fixture".into()).unwrap();
        }
    }

    #[test]
    fn source_version_reply_is_parsed_strictly() {
        assert_eq!(
            parse_source_version("info string x\nsource-version abc-dirty+use-lazy-evaluate\n"),
            Some("abc-dirty+use-lazy-evaluate".into())
        );
        assert_eq!(parse_source_version("id name x\n"), None);
        assert_eq!(parse_source_version("source-version \n"), None);
        assert_eq!(parse_source_version("source-version a b\n"), None);
    }

    #[test]
    fn unusable_versions_are_warned_and_dirty_is_told_apart() {
        assert!(version_warning(Some(VERSION_A)).is_none());
        let none = version_warning(None).unwrap();
        let unknown = version_warning(Some("unknown")).unwrap();
        let dirty = version_warning(Some(&format!("{VERSION_A}-dirty+deep"))).unwrap();
        assert!(unknown.contains("unknown"));
        assert!(dirty.contains("-dirty"));
        assert_ne!(none, unknown);
        assert_ne!(unknown, dirty);
    }

    #[test]
    fn engine_without_the_flag_has_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "not an executable", "model");
        assert_eq!(engine_source_version(&m.engine), None);
        assert_eq!(engine_source_version(&dir.path().join("absent")), None);
    }

    #[test]
    fn version_is_outside_the_fingerprint_and_omitted_when_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "engine", "model");
        let plain = settings(&m, None);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("engine_version").is_none());
        let restored: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(restored, plain);
        let versioned = settings(&m, Some(VERSION_A));
        assert_eq!(versioned.fingerprint, plain.fingerprint);
        let json = serde_json::to_value(&versioned).unwrap();
        assert_eq!(json["engine_version"], VERSION_A);
    }

    #[test]
    fn import_journal_keeps_all_source_conditions_without_admission_flags() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "engine", "model");
        let src = Settings::new(&reading_args(&m, 200, &[])).unwrap();
        let other = src.with_overrides(3, 300).unwrap();
        let dst = Settings::new(&reading_args(&m, 100, &[])).unwrap();
        let source = dir.path().join("src.jsonl");
        let destination = dir.path().join("dst.jsonl");
        write_journal(
            &source,
            &[
                (&src, reading(0, &src, "resign")),
                (&other, reading(0, &other, "win")),
            ],
        );
        write_journal(&destination, &[(&dst, reading(0, &dst, "resign"))]);
        run(ImportJournalArgs {
            from_journal: vec![source],
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: destination.clone(),
            reading: reading_args(&m, 100, &[]),
            force: false,
            on_conflict: OnConflict::Reject,
            conflicts_out: None,
        })
        .unwrap();
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&destination)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1]["settings"], serde_json::to_value(&src).unwrap());
        assert_eq!(rows[2]["settings"], serde_json::to_value(&other).unwrap());
    }

    #[test]
    fn source_is_read_without_modification_and_merged_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let b = machine(dir.path(), "b", "build b", "model");
        let src = settings(&a, None);
        let mut versioned = src.clone();
        versioned.engine_version = Some(VERSION_A.into());
        let other = Settings::new(&reading_args(&a, 300, &[])).unwrap();
        let src_path = dir.path().join("src.jsonl");
        write_journal(
            &src_path,
            &[
                (&src, reading(0, &src, "resign")),
                (&other, reading(1, &other, "resign")),
                (&versioned, reading(2, &versioned, "resign")),
            ],
        );
        let mut bytes = std::fs::read(&src_path).unwrap();
        bytes.extend_from_slice(b"{partial");
        std::fs::write(&src_path, &bytes).unwrap();

        assert_eq!(read_source(&src_path, None, |_| true).unwrap().readings.len(), 3);
        let source = read_source(&src_path, Some(&src.fingerprint), |_| true).unwrap();
        assert!(source.ignored_tail);
        assert_eq!(source.readings.len(), 2);
        assert_eq!(source.reading_settings[0].engine_version.as_deref(), Some(VERSION_A));
        assert_eq!(std::fs::read(&src_path).unwrap(), bytes);
        {
            let _writer = LockedFile::open(&src_path).unwrap();
            assert!(read_source(&src_path, Some(&src.fingerprint), |_| true).is_err());
        }

        let dst = settings(&b, Some(VERSION_A));
        let dst_path = dir.path().join("dst.jsonl");
        write_journal(&dst_path, &[(&dst, reading(0, &dst, "resign"))]);
        let mut journal = Journal::open(&dst_path, &dst.fingerprint).unwrap();
        let incoming: Vec<_> =
            source.readings.iter().map(|r| (r.clone(), "copied".to_owned())).collect();
        let (appended, skipped, _) = journal
            .merge_all(incoming, &dst, OnConflict::Reject, &mut NoConflictList, &BTreeSet::new())
            .unwrap();
        assert_eq!((appended, skipped), (1, 1));
        drop(journal);

        let mut conflicting = reading(3, &dst, "resign");
        let size = std::fs::metadata(&dst_path).unwrap().len();
        let mut journal = Journal::open(&dst_path, &dst.fingerprint).unwrap();
        let mut changed = reading(0, &dst, "resign");
        changed.bestmove = "win".into();
        conflicting.tag = "new".into();
        let incoming = vec![(conflicting, "copied".into()), (changed, "copied".into())];
        assert!(
            journal
                .merge_all(
                    incoming,
                    &dst,
                    OnConflict::Reject,
                    &mut NoConflictList,
                    &BTreeSet::new()
                )
                .is_err()
        );
        assert_eq!(std::fs::metadata(&dst_path).unwrap().len(), size);
        drop(journal);
        let journal = Journal::open(&dst_path, &dst.fingerprint).unwrap();
        let keys: BTreeMap<_, _> = (0..4)
            .map(|c| {
                let key = reading(c, &dst, "resign").key;
                let found = super::super::journal::Readings::get(&journal, &key).is_some();
                (c, found)
            })
            .collect();
        assert_eq!(keys, BTreeMap::from([(0, true), (1, false), (2, true), (3, false)]));
    }

    #[test]
    fn keep_existing_keeps_the_first_value_and_lists_every_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "build a", "model");
        let dst = settings(&m, Some(VERSION_A));
        let path = dir.path().join("dst.jsonl");
        write_journal(&path, &[(&dst, reading(0, &dst, "resign"))]);
        let mut changed = reading(0, &dst, "resign");
        changed.bestmove = "win".into();
        let first = reading(1, &dst, "resign");
        let mut second = reading(1, &dst, "resign");
        second.bestmove = "win".into();
        let incoming = vec![
            (changed, "from-b".to_owned()),
            (reading(0, &dst, "resign"), "same".to_owned()),
            (first, "first".to_owned()),
            (second, "second".to_owned()),
            (reading(2, &dst, "resign"), "new".to_owned()),
        ];
        let mut journal = Journal::open(&path, &dst.fingerprint).unwrap();
        let mut listed = Listed::default();
        let (appended, skipped, conflicts) = journal
            .merge_all(incoming, &dst, OnConflict::KeepExisting, &mut listed, &BTreeSet::new())
            .unwrap();
        assert_eq!((appended, skipped, conflicts), (2, 3, 2));
        assert_eq!(
            listed.rows,
            [
                ("resign".to_owned(), "win".to_owned(), "from-b".to_owned()),
                ("resign".to_owned(), "win".to_owned(), "second".to_owned())
            ]
        );
        assert!(listed.published);
        drop(journal);
        let size = std::fs::metadata(&path).unwrap().len();
        let mut journal = Journal::open(&path, &dst.fingerprint).unwrap();
        let failing = vec![(reading(3, &dst, "resign"), "hook".to_owned())];
        assert!(
            journal
                .merge_all(
                    failing,
                    &dst,
                    OnConflict::KeepExisting,
                    &mut Listed {
                        fail: true,
                        ..Default::default()
                    },
                    &BTreeSet::new(),
                )
                .is_err()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        drop(journal);
        let journal = Journal::open(&path, &dst.fingerprint).unwrap();
        for column in 0..3 {
            let key = reading(column, &dst, "resign").key;
            let kept = super::super::journal::Readings::get(&journal, &key).unwrap();
            assert_eq!(kept.bestmove, "resign");
        }

        let out = dir.path().join("conflicts.tsv");
        let mut tsv = ConflictTsv::create(&out).unwrap();
        let (kept, dropped) = (reading(0, &dst, "resign"), reading(0, &dst, "win"));
        tsv.conflict(&kept, &dropped, "a\tb").unwrap();
        assert!(!out.exists());
        tsv.before_append().unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with(CONFLICT_HEADER));
        assert!(text.lines().nth(1).unwrap().ends_with("\tresign\t\twin\t\ta\\tb"), "{text}");
        let mut empty = ConflictTsv::create(&out).unwrap();
        empty.before_append().unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), CONFLICT_HEADER);
    }

    /// 食い違いを覚えておく受け取り先。`fail` なら追記の前に失敗する。
    #[derive(Default)]
    struct Listed {
        rows: Vec<(String, String, String)>,
        published: bool,
        fail: bool,
    }

    impl ConflictSink for Listed {
        fn conflict(
            &mut self,
            existing: &Reading,
            incoming: &Reading,
            provenance: &str,
        ) -> Result<()> {
            self.rows.push((
                existing.bestmove.clone(),
                incoming.bestmove.clone(),
                provenance.to_owned(),
            ));
            Ok(())
        }

        fn before_append(&mut self) -> Result<()> {
            if self.fail {
                bail!("一覧を書けない");
            }
            self.published = true;
            Ok(())
        }
    }

    #[test]
    fn keep_existing_on_the_command_writes_the_list_before_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "build a", "model");
        let cond = settings(&m, None);
        let src_path = dir.path().join("src.jsonl");
        write_journal(
            &src_path,
            &[
                (&cond, reading(0, &cond, "win")),
                (&cond, reading(1, &cond, "resign")),
            ],
        );
        let dst_path = dir.path().join("dst.jsonl");
        write_journal(&dst_path, &[(&cond, reading(0, &cond, "resign"))]);
        let before = std::fs::read(&dst_path).unwrap();
        let args = |on_conflict, conflicts_out: Option<PathBuf>| ImportJournalArgs {
            from_journal: vec![src_path.clone()],
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: dst_path.clone(),
            reading: reading_args(&m, 100, &["DrawValueBlack=-50"]),
            force: false,
            on_conflict,
            conflicts_out,
        };
        let out = dir.path().join("conflicts.tsv");
        assert!(run(args(OnConflict::Reject, None)).is_err());
        assert!(run(args(OnConflict::KeepExisting, None)).is_err());
        assert!(run(args(OnConflict::Reject, Some(out.clone()))).is_err());
        assert!(run(args(OnConflict::KeepExisting, Some(dst_path.clone()))).is_err());
        assert!(run(args(OnConflict::KeepExisting, Some(src_path.clone()))).is_err());
        assert!(run(args(OnConflict::KeepExisting, Some(dir.path().join("none/c.tsv")))).is_err());
        assert_eq!(std::fs::read(&dst_path).unwrap(), before);

        run(args(OnConflict::KeepExisting, Some(out.clone()))).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let rows: Vec<_> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with(&format!("{}\t1\tresign\t", reading(0, &cond, "win").key)));
        let journal = Journal::open(&dst_path, &cond.fingerprint).unwrap();
        let get = |c| {
            super::super::journal::Readings::get(&journal, &reading(c, &cond, "resign").key)
                .cloned()
        };
        assert_eq!(get(0).unwrap().bestmove, "resign");
        assert_eq!(get(1).unwrap().bestmove, "resign");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with(".tmp")), "{names:?}");
        drop(journal);

        // 取り込み先の台帳の dir がまだ無くても、出力先との照合で失敗しない。
        let fresh = dir.path().join("new-dir/dst.jsonl");
        let mut fresh_args = args(OnConflict::KeepExisting, Some(dir.path().join("fresh.tsv")));
        fresh_args.journal = fresh.clone();
        run(fresh_args).unwrap();
        assert!(fresh.exists());
    }

    #[test]
    fn conflict_fields_are_escaped() {
        assert_eq!(escape_tsv("a\tb\nc\rd\\e"), "a\\tb\\nc\\rd\\\\e");
    }

    /// serve が書いた台帳の条件 (ファイルのパスだけ置き換えたもの) と、そのとき記録された指紋。
    const RECORDED_SETTINGS: &str = r#"{"fingerprint":"74695922da474f8e2284c42755db2d93b0fb80815b63a253a1ae071c6ac8942a","version":"book-tree-v1/newgame-ready/node-calib-last-multipv-v1","engine_sha256":"8c32f9585e25eb773b008e05fcd03cd7c6e590660b5eda0a561e3746ce877984","options":{"DrawValueBlack":"-70","DrawValueWhite":"70","EvalFile":"/path/to/model.bin","FV_SCALE":"14","LS_BUCKET_MODE":"progresskpabs","LS_PROGRESS_BUCKETS":"8","LS_PROGRESS_COEFF":"/path/to/progress.bin","MultiPV":"3","Threads":"1","USI_Hash":"1024","UseEvalHash":"false"},"content_sha256":{"EvalFile":"7871f1c0b7af35d84e76a713b20e7b313f8294167d513761f0a69b4e24c4003d","LS_PROGRESS_COEFF":"8d0058b899768c00e71be5508c5c4544d8c8ce24b30c32024c2a0a4452dbdb53","implicit:SPSAParamsFile":"absent"},"nodes":1000000000,"multipv":3,"hash":1024}"#;

    #[test]
    fn import_journal_two_conditions_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "a", "build", "model");
        let a = settings(&m, None);
        let b = a.with_overrides(a.multipv, a.nodes + 1).unwrap();
        let source = dir.path().join("source.jsonl");
        write_journal(
            &source,
            &[
                (&a, reading(0, &a, "resign")),
                (&b, reading(0, &b, "win")),
                (&a, reading(1, &a, "resign")),
                (&b, reading(1, &b, "win")),
            ],
        );
        let destination = dir.path().join("destination.jsonl");
        let args = || ImportJournalArgs {
            from_journal: vec![source.clone()],
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: destination.clone(),
            reading: reading_args(&m, 100, &[]),
            force: false,
            on_conflict: OnConflict::Reject,
            conflicts_out: None,
        };
        assert_eq!(import(args()).unwrap(), (4, 0, 0));
        let bytes = std::fs::read(&destination).unwrap();
        assert_eq!(import(args()).unwrap(), (0, 4, 0));
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    }

    fn import_args(m: &Machine, sources: &[&Path], destination: &Path) -> ImportJournalArgs {
        ImportJournalArgs {
            from_journal: sources.iter().map(|p| p.to_path_buf()).collect(),
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: destination.to_path_buf(),
            reading: reading_args(m, 100, &[]),
            force: false,
            on_conflict: OnConflict::Reject,
            conflicts_out: None,
        }
    }

    fn offsets(path: &Path) -> OffsetState {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn offsets_reject_missing_fingerprint_and_unknown_fields_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        let args = || {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args
        };
        import(args()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        assert!(saved["fingerprint"].is_null());
        let before = std::fs::read(&dst).unwrap();
        for target in [
            "fingerprint",
            "destination_conflicts",
            "root",
            "destination",
            "source",
        ] {
            let mut invalid = saved.clone();
            let expected = if target == "fingerprint" {
                invalid.as_object_mut().unwrap().remove("fingerprint");
                "missing field `fingerprint`"
            } else if target == "destination_conflicts" {
                invalid["sources"]
                    .as_object_mut()
                    .unwrap()
                    .values_mut()
                    .next()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("destination_conflicts");
                "missing field `destination_conflicts`"
            } else {
                let object = match target {
                    "destination" => invalid["destination"].as_object_mut().unwrap(),
                    "source" => invalid["sources"]
                        .as_object_mut()
                        .unwrap()
                        .values_mut()
                        .next()
                        .unwrap()
                        .as_object_mut()
                        .unwrap(),
                    _ => invalid.as_object_mut().unwrap(),
                };
                object.insert("wip_field".into(), true.into());
                "unknown field `wip_field`"
            };
            let bytes = serde_json::to_vec(&invalid).unwrap();
            std::fs::write(&state, &bytes).unwrap();
            let err = format!("{:#}", import(args()).unwrap_err());
            assert!(err.contains(expected), "{err}");
            assert!(err.contains("再開状態を読めません"), "{err}");
            assert!(err.contains("offsets ファイルを削除"), "{err}");
            assert_eq!(std::fs::read(&dst).unwrap(), before);
            assert_eq!(std::fs::read(&state).unwrap(), bytes);
        }
    }

    #[test]
    fn fingerprint_filter_changes_reject_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let mut other = cond.clone();
        other.nodes += 1;
        other.fingerprint = other.computed_fingerprint().unwrap();
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        write_journal(
            &src,
            &[
                (&cond, reading(0, &cond, "resign")),
                (&other, reading(1, &other, "resign")),
            ],
        );
        let args = |fp: Option<String>| {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args.fingerprint = fp;
            args
        };
        assert_eq!(import(args(Some(cond.fingerprint.clone()))).unwrap(), (1, 0, 0));
        let before = std::fs::read(&dst).unwrap();
        let saved = std::fs::read(&state).unwrap();
        for fp in [None, Some(other.fingerprint)] {
            let err = import(args(fp)).unwrap_err().to_string();
            assert!(
                err.contains("指紋フィルタ") && err.contains("offsets ファイルを削除"),
                "{err}"
            );
            assert_eq!(std::fs::read(&dst).unwrap(), before);
            assert_eq!(std::fs::read(&state).unwrap(), saved);
        }
    }

    #[test]
    fn source_internal_conflict_across_slices_advances_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        let args = || {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args
        };
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        assert_eq!(import(args()).unwrap(), (1, 0, 0));
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        let entry = saved["sources"].as_object().unwrap().values().next().unwrap();
        assert_eq!(entry.as_object().unwrap().len(), 3);
        assert_eq!(entry["destination_conflicts"], serde_json::json!([]));
        assert!(entry.get("offset").is_some());
        assert!(entry.get("sig").is_some());
        let mut file = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        writeln!(file, "{}", record_json(&cond, &reading(0, &cond, "win"))).unwrap();
        let old = std::fs::read(&dst).unwrap();
        assert_eq!(import(args()).unwrap(), (0, 0, 0));
        assert_eq!(std::fs::read(&dst).unwrap(), old);
        assert_eq!(
            offsets(&state).sources[&std::fs::canonicalize(&src).unwrap()].offset,
            std::fs::metadata(&src).unwrap().len()
        );
        assert_eq!(import(args()).unwrap(), (0, 0, 0));
        let mut keep = args();
        keep.on_conflict = OnConflict::KeepExisting;
        let list = dir.path().join("conflicts.tsv");
        keep.conflicts_out = Some(list.clone());
        writeln!(file, "{}", record_json(&cond, &reading(0, &cond, "win"))).unwrap();
        assert_eq!(import(keep).unwrap(), (0, 0, 0));
        assert_eq!(std::fs::read_to_string(list).unwrap(), CONFLICT_HEADER);
        assert_eq!(read_source(&src, None, |_| true).unwrap().readings.len(), 1);
    }

    #[test]
    fn slice_matching_source_first_value_remains_destination_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        let list = dir.path().join("conflicts.tsv");
        let args = |keep| {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            if keep {
                args.on_conflict = OnConflict::KeepExisting;
                args.conflicts_out = Some(list.clone());
            }
            args
        };
        write_journal(&dst, &[(&cond, reading(0, &cond, "resign"))]);
        write_journal(&src, &[(&cond, reading(0, &cond, "win"))]);
        assert_eq!(import(args(true)).unwrap(), (0, 1, 1));
        let saved = std::fs::read(&state).unwrap();
        let old = std::fs::read(&dst).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        writeln!(file, "{}", record_json(&cond, &reading(0, &cond, "resign"))).unwrap();
        writeln!(file, "{}", record_json(&cond, &reading(0, &cond, "win"))).unwrap();
        assert!(import(args(false)).unwrap_err().to_string().contains("値の違う重複"));
        assert_eq!(std::fs::read(&state).unwrap(), saved);
        assert_eq!(std::fs::read(&dst).unwrap(), old);
        assert_eq!(import(args(true)).unwrap(), (0, 1, 1));
        let conflicts = std::fs::read_to_string(&list).unwrap();
        assert_eq!(conflicts.lines().count(), 2);
        let fields: Vec<_> = conflicts.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(fields[2], "resign");
        assert_eq!(fields[4], "win");
        assert_eq!(
            offsets(&state).sources[&std::fs::canonicalize(&src).unwrap()].offset,
            std::fs::metadata(&src).unwrap().len()
        );
        assert_eq!(std::fs::read(&dst).unwrap(), old);
    }

    #[test]
    fn rejected_source_first_value_blocks_destination_equal_slice_prefetch() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let full_dst = dir.path().join("full.jsonl");
        let state = dir.path().join("offsets.json");
        let list = dir.path().join("conflicts.tsv");
        let args = || {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args.on_conflict = OnConflict::KeepExisting;
            args.conflicts_out = Some(list.clone());
            args
        };
        let a = reading(0, &cond, "resign");
        let b = reading(0, &cond, "win");
        write_journal(&dst, &[(&cond, a.clone())]);
        write_journal(&src, &[(&cond, b)]);
        let before = std::fs::read(&dst).unwrap();
        std::fs::write(&full_dst, &before).unwrap();
        assert_eq!(import(args()).unwrap(), (0, 1, 1));
        let canonical = std::fs::canonicalize(&src).unwrap();
        let identity = source_identity(&cond.fingerprint, &a.key).unwrap();
        assert_eq!(identity.len(), 32);
        assert!(identity.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            offsets(&state).sources[&canonical].destination_conflicts,
            BTreeSet::from([identity])
        );
        let mut rec = record_json(&cond, &a);
        rec["prefetch"] = true.into();
        let mut file = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        writeln!(file, "{rec}").unwrap();
        drop(file);
        assert_eq!(import(args()).unwrap(), (0, 0, 0));
        assert_eq!(std::fs::read(&dst).unwrap(), before);
        assert_eq!(std::fs::read_to_string(&list).unwrap(), CONFLICT_HEADER);
        let mut full = args();
        full.offsets = None;
        full.journal = full_dst.clone();
        assert_eq!(import(full).unwrap(), (0, 1, 1));
        assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&full_dst).unwrap());
        assert!(read_source(&src, None, |_| true).unwrap().prefetched.is_empty());

        write_lines(&src, &[record_json(&cond, &a)]);
        assert_eq!(import(args()).unwrap(), (0, 1, 0));
        assert!(offsets(&state).sources[&canonical].destination_conflicts.is_empty());
    }

    #[test]
    fn slice_without_candidates_does_not_reopen_source() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let row = reading(0, &cond, "resign");
        write_journal(&src, &[(&cond, row.clone())]);
        write_journal(&dst, &[(&cond, row)]);
        let prefix = read_source(&src, None, |_| true).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        for _ in 0..2 {
            writeln!(file, "{}", record_json(&cond, &reading(0, &cond, "resign"))).unwrap();
        }
        drop(file);
        let mut slice = read_source_incremental(
            &src,
            None,
            |_| true,
            Some((prefix.end, &prefix.sig)),
            |_| false,
        )
        .unwrap();
        std::fs::remove_file(&src).unwrap();
        let journal = Journal::open(&dst, &cond.fingerprint).unwrap();
        slice
            .skip_prefix_conflicts(&src, &journal, &BTreeSet::new(), |_| false)
            .unwrap();
        assert_eq!(slice.readings.len(), 1);
    }

    #[test]
    fn offsets_are_read_only_after_destination_lock() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        std::fs::write(&state, b"invalid state").unwrap();
        let lock = LockedFile::open(&dst).unwrap();
        let mut args = import_args(&m, &[&src], &dst);
        args.offsets = Some(state);
        let err = import(args).unwrap_err().to_string();
        assert!(err.contains("lock 取得失敗"), "{err}");
        drop(lock);
    }

    #[test]
    fn rewritten_source_uses_first_value_from_new_contents() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        let args = || {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args
        };
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        import(args()).unwrap();
        write_lines(&src, &[record_json(&cond, &reading(0, &cond, "win"))]);
        let old_state = std::fs::read(&state).unwrap();
        let old_dst = std::fs::read(&dst).unwrap();
        assert!(import(args()).unwrap_err().to_string().contains("値の違う重複"));
        assert_eq!(std::fs::read(&state).unwrap(), old_state);
        assert_eq!(std::fs::read(&dst).unwrap(), old_dst);
    }

    #[test]
    fn missing_offsets_directory_fails_before_append() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        write_journal(&src, &[(&cond, reading(1, &cond, "resign"))]);
        write_journal(&dst, &[(&cond, reading(0, &cond, "resign"))]);
        let old = std::fs::read(&dst).unwrap();
        let mut args = import_args(&m, &[&src], &dst);
        args.offsets = Some(dir.path().join("missing/offsets.json"));
        assert!(import(args).is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), old);
    }

    #[test]
    fn torn_only_source_is_empty_for_full_reads_but_not_a_saved_slice() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jsonl");
        std::fs::write(&src, b"torn").unwrap();
        assert!(
            read_source(&src, None, |_| true)
                .err()
                .unwrap()
                .to_string()
                .contains("取り込み元の台帳が空です")
        );
        assert!(read_source_incremental(&src, None, |_| true, None, |_| false).is_err());
        let sig = offset_signature(&mut std::fs::File::open(&src).unwrap(), 0).unwrap();
        assert!(
            read_source_incremental(&src, None, |_| true, Some((0, &sig)), |_| false)
                .unwrap()
                .readings
                .is_empty()
        );
    }

    #[test]
    fn tail_window_change_outside_head_resets_offset_and_reports_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let mut rows = Vec::new();
        for _ in 0..20 {
            rows.push(record_json(&cond, &reading(0, &cond, "resign")));
        }
        write_lines(&src, &rows);
        let mut bytes = std::fs::read(&src).unwrap();
        assert!(bytes.len() > 8192);
        let source = read_source_incremental(&src, None, |_| true, None, |_| false).unwrap();
        let needle = b"fixture";
        let index = bytes
            .windows(needle.len())
            .enumerate()
            .find_map(|(i, b)| (i > 4096 && i >= bytes.len() - 4096 && b == needle).then_some(i))
            .unwrap();
        bytes[index] = b'F';
        std::fs::write(&src, bytes).unwrap();
        let reread = read_source_incremental(
            &src,
            None,
            |_| true,
            Some((source.end, &source.sig)),
            |_| false,
        )
        .unwrap();
        assert!(reread.rewritten);
        assert_eq!(reread.start, 0);
        assert_eq!(
            source_report(
                Path::new("source.jsonl"),
                reread.start..reread.end,
                (0, 1, 0),
                2,
                reread.rewritten
            ),
            format!(
                "source=source.jsonl start=0 end={} appended=0 skipped=1 conflicts=0 excluded=2 rewritten=1",
                source.end
            )
        );
        assert_eq!(
            source_report(Path::new("source.jsonl"), 10..20, (1, 2, 3), 4, false),
            "source=source.jsonl start=10 end=20 appended=1 skipped=2 conflicts=3 excluded=4"
        );
    }

    #[test]
    fn multiple_sources_equal_sequential_imports_with_conflicts_and_prefetch() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let together = dir.path().join("together.jsonl");
        let sequential = dir.path().join("sequential.jsonl");
        write_journal(
            &a,
            &[
                (&cond, reading(0, &cond, "resign")),
                (&cond, reading(1, &cond, "resign")),
            ],
        );
        let mut marked = record_json(&cond, &reading(1, &cond, "resign"));
        marked["prefetch"] = true.into();
        write_lines(
            &b,
            &[
                record_json(&cond, &reading(0, &cond, "win")),
                marked,
                record_json(&cond, &reading(2, &cond, "resign")),
            ],
        );
        let mut combined = import_args(&m, &[&a, &b], &together);
        combined.on_conflict = OnConflict::KeepExisting;
        combined.conflicts_out = Some(dir.path().join("combined.tsv"));
        let counts = import(combined).unwrap();
        let mut sums = (0, 0, 0);
        for (i, source) in [&a, &b].iter().enumerate() {
            let mut args = import_args(&m, &[source], &sequential);
            args.on_conflict = OnConflict::KeepExisting;
            args.conflicts_out = Some(dir.path().join(format!("{i}.tsv")));
            let result = import(args).unwrap();
            sums.0 += result.0;
            sums.1 += result.1;
            sums.2 += result.2;
        }
        assert_eq!(counts, (3, 2, 1));
        assert_eq!(counts, sums);
        assert_eq!(std::fs::read(&together).unwrap(), std::fs::read(&sequential).unwrap());
        let list = std::fs::read_to_string(dir.path().join("combined.tsv")).unwrap();
        assert_eq!(list.lines().count(), 2);
        assert!(list.contains(&format!("path={}", b.display())));
        let journal = Journal::open(&together, &cond.fingerprint).unwrap();
        assert!(journal.prefetched.contains(&reading(1, &cond, "resign").key));
    }

    #[test]
    fn offsets_import_only_new_complete_lines_and_preserve_other_sources() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        write_journal(&a, &[(&cond, reading(0, &cond, "resign"))]);
        write_journal(&b, &[(&cond, reading(3, &cond, "resign"))]);
        let args = |sources: &[&Path]| {
            let mut args = import_args(&m, sources, &dst);
            args.offsets = Some(state.clone());
            args
        };
        assert_eq!(import(args(&[&a, &b])).unwrap(), (2, 0, 0));
        let start = std::fs::metadata(&a).unwrap().len();
        let next = format!("{}\n", record_json(&cond, &reading(1, &cond, "resign")));
        let torn = format!("{}", record_json(&cond, &reading(2, &cond, "resign")));
        let mut file = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        write!(file, "{next}{torn}").unwrap();
        assert_eq!(import(args(&[&a])).unwrap(), (1, 0, 0));
        let saved = offsets(&state);
        let entry = &saved.sources[&std::fs::canonicalize(&a).unwrap()];
        assert_eq!(entry.offset, start + next.len() as u64);
        assert_eq!(saved.sources.len(), 2);
        let last: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&dst).unwrap().lines().last().unwrap())
                .unwrap();
        assert!(last["provenance"].as_str().unwrap().ends_with(&format!(":offset={start}")));
        assert!(
            last["provenance"]
                .as_str()
                .unwrap()
                .contains(&format!("sha256={:x}", Sha256::digest(next.as_bytes())))
        );
        assert_eq!(import(args(&[&a])).unwrap(), (0, 0, 0));
        writeln!(file).unwrap();
        assert_eq!(import(args(&[&a])).unwrap(), (1, 0, 0));
        assert_eq!(import(args(&[&a])).unwrap(), (0, 0, 0));
    }

    #[test]
    fn unchanged_slice_never_parses_old_rows_and_rewrites_reset_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.jsonl");
        // 解析できない既存行でも、有効な保存位置より前なら読まない。
        std::fs::write(&path, b"not json\nsecond invalid row\n").unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        let sig = offset_signature(&mut std::fs::File::open(&path).unwrap(), len).unwrap();
        let source = read_source_incremental(
            &path,
            None,
            |_| true,
            Some((len, &sig)),
            |_| panic!("解析不要"),
        )
        .unwrap();
        assert!(source.readings.is_empty());
        assert_eq!((source.start, source.end, source.rewritten), (len, len, false));

        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        write_lines(
            &path,
            &[
                record_json(&cond, &reading(0, &cond, "resign")),
                record_json(&cond, &reading(1, &cond, "resign")),
            ],
        );
        let original = std::fs::read(&path).unwrap();
        let source = read_source_incremental(&path, None, |_| true, None, |_| false).unwrap();
        let starts: Vec<_> = original
            .windows(b"\"provenance\":\"fixture".len())
            .enumerate()
            .filter_map(|(i, bytes)| {
                (bytes == b"\"provenance\":\"fixture").then_some(i + b"\"provenance\":\"".len())
            })
            .collect();
        assert_eq!(starts.len(), 2);
        for index in starts {
            let mut bytes = original.clone();
            bytes[index] = b'F';
            std::fs::write(&path, bytes).unwrap();
            let reread = read_source_incremental(
                &path,
                None,
                |_| true,
                Some((source.end, &source.sig)),
                |_| false,
            )
            .unwrap();
            assert!(reread.rewritten);
            assert_eq!(reread.start, 0);
            assert_eq!(reread.readings.len(), 2);
        }
        write_lines(&path, &[record_json(&cond, &reading(0, &cond, "resign"))]);
        assert!(
            read_source_incremental(
                &path,
                None,
                |_| true,
                Some((source.end, &source.sig)),
                |_| false
            )
            .unwrap()
            .rewritten
        );
    }

    #[test]
    fn excluded_digests_distinguish_seconds_and_normalize_signed_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exclude.jsonl");
        std::fs::write(
            &path,
            "{\"key\":\"same\",\"ply\":1,\"nodes\":100,\"sec\":0.125}\n\
             {\"key\":\"same\",\"ply\":1,\"nodes\":100,\"sec\":0.25}\n",
        )
        .unwrap();
        let (digests, _) = load_excluded(Some(&path)).unwrap();
        assert_eq!(digests.len(), 2);
        for sec in [0.125, 0.25] {
            assert!(
                digests
                    .binary_search(&ExcludedReading::digest("same", 1, Some(100), sec))
                    .is_ok()
            );
        }
        assert!(
            digests
                .binary_search(&ExcludedReading::digest("same", 1, Some(100), 0.5))
                .is_err()
        );
        assert_eq!(
            ExcludedReading::digest("same", 1, Some(100), -0.0),
            ExcludedReading::digest("same", 1, Some(100), 0.0)
        );
    }

    #[test]
    fn excluded_readings_match_exact_identity_before_deduplication() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let list = dir.path().join("exclude.jsonl");
        let excluded = reading(0, &cond, "resign");
        let mut kept = excluded.clone();
        kept.sec += 0.001;
        kept.bestmove = "win".into();
        write_lines(&src, &[record_json(&cond, &excluded), record_json(&cond, &kept)]);
        std::fs::write(&list, format!("\n{}\n", serde_json::json!({"key":excluded.key,"ply":excluded.ply,"nodes":excluded.nodes,"sec":excluded.sec}))).unwrap();
        let (set, _) = load_excluded(Some(&list)).unwrap();
        let source = read_source_incremental(
            &src,
            None,
            |_| true,
            None,
            |r| set.binary_search(&ExcludedReading::identity(r)).is_ok(),
        )
        .unwrap();
        assert_eq!(source.excluded, 1);
        assert_eq!(source.readings, [kept]);
        let mut args = import_args(&m, &[&src], &dst);
        args.exclude_readings = Some(list.clone());
        assert_eq!(import(args).unwrap(), (1, 0, 0));
        std::fs::write(&list, "{}\n").unwrap();
        assert!(load_excluded(Some(&list)).is_err());
    }

    #[test]
    fn destination_and_exclusion_mismatch_leave_journal_and_offsets_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        let excluded = dir.path().join("excluded.jsonl");
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        std::fs::write(&excluded, "\n").unwrap();
        let args = || {
            let mut args = import_args(&m, &[&src], &dst);
            args.offsets = Some(state.clone());
            args.exclude_readings = Some(excluded.clone());
            args
        };
        import(args()).unwrap();
        let saved = std::fs::read(&state).unwrap();
        let original = std::fs::read(&dst).unwrap();
        std::fs::write(&excluded, "\n\n").unwrap();
        let err = import(args()).unwrap_err().to_string();
        assert!(err.contains("offsets ファイルを削除"), "{err}");
        assert_eq!(std::fs::read(&dst).unwrap(), original);
        std::fs::write(&excluded, "\n").unwrap();
        let other = dir.path().join("other.jsonl");
        std::fs::write(&other, &original).unwrap();
        let mut changed = args();
        changed.journal = other.clone();
        assert!(import(changed).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), original);
        for bytes in [Vec::new(), [b" ".as_slice(), original.as_slice()].concat()] {
            std::fs::write(&dst, &bytes).unwrap();
            let err = import(args()).unwrap_err().to_string();
            assert!(err.contains("offsets ファイルを削除"), "{err}");
            assert_eq!(std::fs::read(&dst).unwrap(), bytes);
            assert_eq!(std::fs::read(&state).unwrap(), saved);
        }
    }

    #[test]
    fn second_source_conflict_rejects_every_append_and_preserves_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let dst = dir.path().join("dst.jsonl");
        let state = dir.path().join("offsets.json");
        write_journal(&a, &[(&cond, reading(0, &cond, "resign"))]);
        let args = |sources: &[&Path]| {
            let mut args = import_args(&m, sources, &dst);
            args.offsets = Some(state.clone());
            args
        };
        import(args(&[&a])).unwrap();
        let saved = std::fs::read(&state).unwrap();
        let original = std::fs::read(&dst).unwrap();
        write_journal(&a, &[(&cond, reading(1, &cond, "resign"))]);
        write_journal(&b, &[(&cond, reading(1, &cond, "win"))]);
        assert!(import(args(&[&a, &b])).is_err());
        assert_eq!(std::fs::read(&state).unwrap(), saved);
        assert_eq!(std::fs::read(&dst).unwrap(), original);
    }

    #[test]
    fn duplicate_canonical_source_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "m", "engine", "model");
        let cond = settings(&m, None);
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("dst.jsonl");
        write_journal(&src, &[(&cond, reading(0, &cond, "resign"))]);
        let alias = dir.path().join(".").join("src.jsonl");
        let err = import(import_args(&m, &[&src, &alias], &dst)).unwrap_err();
        assert!(err.to_string().contains("重複"));
        assert!(!dst.exists());
    }

    #[test]
    fn recorded_settings_keep_their_fingerprint() {
        let mut settings: Settings = serde_json::from_str(RECORDED_SETTINGS).unwrap();
        assert_eq!(settings.engine_version, None);
        assert_eq!(settings.computed_fingerprint().unwrap(), settings.fingerprint);
        settings.engine_version = Some(VERSION_A.into());
        assert_eq!(settings.computed_fingerprint().unwrap(), settings.fingerprint);
        let json = serde_json::to_string(&Settings {
            engine_version: None,
            ..settings
        })
        .unwrap();
        assert_eq!(json, RECORDED_SETTINGS);
    }

    fn write_lines(path: &Path, records: &[serde_json::Value]) {
        let text: String = records.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    fn record_json(settings: &Settings, reading: &Reading) -> serde_json::Value {
        serde_json::json!({
            "kind": settings.version,
            "settings": settings,
            "provenance": "fixture",
            "reading": reading,
        })
    }

    #[test]
    fn filtered_out_positions_skip_conflicting_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let src = settings(&a, None);
        let path = dir.path().join("src.jsonl");
        let kept = reading(1, &src, "resign");
        let keep = |key: &str| key == kept.key;
        let lines = |second: &str| {
            [
                record_json(&src, &reading(0, &src, "resign")),
                record_json(&src, &reading(0, &src, second)),
                record_json(&src, &kept),
            ]
        };
        write_lines(&path, &lines("resign"));
        let source = read_source(&path, None, keep).unwrap();
        assert_eq!(source.readings, std::slice::from_ref(&kept));
        write_lines(&path, &lines("win"));
        assert_eq!(read_source(&path, None, keep).unwrap().readings, [kept]);
    }

    #[test]
    fn source_skips_invalid_rows_and_accepts_version_differences() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let src = settings(&a, None);
        let path = dir.path().join("src.jsonl");
        let good = record_json(&src, &reading(0, &src, "resign"));

        let mut tampered = src.clone();
        tampered.nodes = 200;
        let lying = record_json(&tampered, &reading(1, &tampered, "resign"));
        write_lines(&path, &[good.clone(), lying.clone()]);
        assert_eq!(read_source(&path, None, |_| true).unwrap().readings.len(), 1);
        write_lines(&path, &[lying]);
        assert!(read_source(&path, None, |_| true).unwrap().readings.is_empty());
        assert!(
            read_source(&path, Some(&src.fingerprint), |_| true)
                .unwrap()
                .readings
                .is_empty()
        );

        // モデルを別のパスへ移した後の行は同じ条件として受け入れる。
        let moved_dir = dir.path().join("moved");
        std::fs::create_dir_all(&moved_dir).unwrap();
        std::fs::copy(&a.model, moved_dir.join("model.bin")).unwrap();
        let moved = Settings::new(&ReadingArgs {
            engine_options: vec![
                format!("EvalFile={}", moved_dir.join("model.bin").display()),
                "DrawValueBlack=-50".into(),
            ],
            ..reading_args(&a, 100, &[])
        })
        .unwrap();
        assert_ne!(moved.options, src.options);
        assert_eq!(moved.fingerprint, src.fingerprint);
        write_lines(
            &path,
            &[
                good.clone(),
                record_json(&moved, &reading(3, &moved, "resign")),
            ],
        );
        assert_eq!(read_source(&path, None, |_| true).unwrap().readings.len(), 2);
        let mut changed_draw = src.clone();
        changed_draw.options.insert("DrawValueBlack".into(), "0".into());
        write_lines(
            &path,
            &[
                good.clone(),
                record_json(&changed_draw, &reading(3, &src, "resign")),
            ],
        );
        assert_eq!(read_source(&path, None, |_| true).unwrap().readings.len(), 1);

        let other = Settings::new(&reading_args(&a, 300, &[])).unwrap();
        let versioned = |v: &str| {
            let mut s = other.clone();
            s.engine_version = Some(v.into());
            record_json(&s, &reading(2, &s, "resign"))
        };
        write_lines(&path, &[good.clone(), versioned(VERSION_A), versioned(VERSION_B)]);
        assert_eq!(read_source(&path, Some(&src.fingerprint), |_| true).unwrap().readings.len(), 1);
        assert_eq!(
            read_source(&path, Some(&other.fingerprint), |_| true).unwrap().readings.len(),
            1
        );

        std::fs::write(&path, "").unwrap();
        let err = read_source(&path, None, |_| true).err().unwrap().to_string();
        assert!(err.contains("取り込み元の台帳が空です"), "{err}");
        let missing = dir.path().join("missing.jsonl");
        let err = format!("{:#}", read_source(&missing, None, |_| true).err().unwrap());
        assert!(err.contains("missing.jsonl"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn version_query_gives_up_when_a_child_keeps_stdout_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::write(&engine, "#!/bin/sh\necho 'source-version abc'\nsleep 30 &\nexit 0\n")
            .unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = Instant::now();
        assert_eq!(query_source_version(&engine, Duration::from_secs(1)), None);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn import_journal_records_source_provenance_across_versions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let engine_reporting = |name: &str, version: &str| {
            let m = machine(dir.path(), name, "", "model");
            std::fs::write(&m.engine, format!("#!/bin/sh\necho 'source-version {version}'\n"))
                .unwrap();
            std::fs::set_permissions(&m.engine, std::fs::Permissions::from_mode(0o755)).unwrap();
            m
        };
        let a = engine_reporting("a", VERSION_A);
        let b = engine_reporting("b", VERSION_B);
        let src_args = reading_args(&a, 100, &[]);
        let mut src = Settings::new(&src_args).unwrap();
        src.engine_version = engine_source_version(&a.engine);
        assert_eq!(src.engine_version.as_deref(), Some(VERSION_A));
        let src_path = dir.path().join("src.jsonl");
        write_journal(&src_path, &[(&src, reading(0, &src, "resign"))]);
        let dst_path = dir.path().join("dst.jsonl");
        let args = |force| ImportJournalArgs {
            from_journal: vec![src_path.clone()],
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: dst_path.clone(),
            reading: reading_args(&b, 100, &[]),
            force,
            on_conflict: OnConflict::Reject,
            conflicts_out: None,
        };
        run(args(false)).unwrap();
        run(args(true)).unwrap();
        let line = std::fs::read_to_string(&dst_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let sha = super::super::journal::digest(&src_path).unwrap();
        assert_eq!(
            record["provenance"],
            format!(
                "journal:sha256={sha}:fingerprint={}:force=false:path={}",
                src.fingerprint,
                src_path.display()
            )
        );
        assert_eq!(record["settings"]["engine_version"], VERSION_A);
        assert!(
            run(ImportJournalArgs {
                journal: src_path.clone(),
                ..args(true)
            })
            .is_err()
        );
        let linked = dir.path().join("linked.jsonl");
        std::fs::hard_link(&src_path, &linked).unwrap();
        let before = std::fs::read(&src_path).unwrap();
        let err = run(ImportJournalArgs {
            journal: linked,
            ..args(true)
        })
        .err()
        .unwrap();
        assert!(err.to_string().contains("同じ台帳"), "{err}");
        assert_eq!(std::fs::read(&src_path).unwrap(), before);
    }
    #[test]
    fn import_same_fingerprint_with_different_versions_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine(dir.path(), "engine", "build", "model");
        let dst = settings(&m, Some(VERSION_A));
        let src = settings(&m, Some(VERSION_B));
        assert_eq!(dst.fingerprint, src.fingerprint);
        let source = dir.path().join("source.jsonl");
        let destination = dir.path().join("destination.jsonl");
        let original = reading(0, &dst, "resign");
        let added = [reading(1, &src, "resign"), reading(2, &src, "resign")];
        write_journal(&destination, &[(&dst, original.clone())]);
        write_journal(&source, &[(&src, added[0].clone()), (&src, added[1].clone())]);
        let counts = import(ImportJournalArgs {
            from_journal: vec![source],
            offsets: None,
            exclude_readings: None,
            fingerprint: None,
            accept_fingerprint: Vec::new(),
            journal: destination.clone(),
            reading: reading_args(&m, 100, &[]),
            force: false,
            on_conflict: OnConflict::Reject,
            conflicts_out: None,
        })
        .unwrap();
        assert_eq!(counts, (2, 0, 0));
        let journal = Journal::open(&destination, &dst.fingerprint).unwrap();
        for r in [&original, &added[0], &added[1]] {
            assert_eq!(journal.get_exact(&r.key, &dst.fingerprint), Some(r));
        }
        assert!(journal.skipped_rows.is_empty());
    }
}
