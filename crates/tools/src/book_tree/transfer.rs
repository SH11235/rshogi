//! 別の台帳の読みを、読みごとの条件を保って取り込む。

use super::ReadingArgs;
use super::journal::{
    ConflictSink, Journal, NoConflictList, OnConflict, Reading, Settings, read_source,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Args, Debug)]
pub(super) struct ImportJournalArgs {
    /// 取り込み元の台帳。書き込み中の台帳は拒否する (複製してから渡す)
    #[arg(long)]
    from_journal: PathBuf,
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
    /// 取り込み先と値の違う重複の扱い。keep-existing は取り込み先の値を残す (取り込み元の中の食い違いは常に拒否)
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
    if let Some(out) = &args.conflicts_out {
        for journal in [&args.journal, &args.from_journal] {
            if same_path(out, journal)? {
                bail!("--conflicts-out が台帳 ({}) と同じです", journal.display());
            }
        }
    }
    let mut dst = Settings::new(&args.reading)?;
    dst.engine_version = engine_source_version(&args.reading.engine);
    warn_unknown_version(&args.reading.engine, &dst.engine_version);
    let source = read_source(&args.from_journal, args.fingerprint.as_deref(), |_| true)?;
    // hard link はパスを正規化しても別に見えるので、ファイルの実体 (device と inode 等) でも比べる。
    if args.journal.try_exists()?
        && (std::fs::canonicalize(&args.journal)? == std::fs::canonicalize(&args.from_journal)?
            || same_file::is_same_file(&args.journal, &args.from_journal)?)
    {
        bail!("取り込み元と取り込み先が同じ台帳です");
    }
    if source.ignored_tail {
        eprintln!("取り込み元の末尾の改行で終わらない行は無視しました");
    }
    // 一覧の一時ファイルは台帳を開く前に作り、検査しながら書く。公開は読みを追記する前に行い、
    // 書けなければ読みを追記せずに失敗する (末尾の書きかけの行の切り詰めは、台帳を開くときに行う)。
    let mut list = match &args.conflicts_out {
        Some(path) => Some(
            ConflictTsv::create(path)
                .with_context(|| format!("食い違いの一覧を {} に作れません", path.display()))?,
        ),
        None => None,
    };
    let mut journal = Journal::open(&args.journal, &dst.fingerprint)?;
    let source_fingerprints = source
        .reading_settings
        .iter()
        .map(|s| s.fingerprint.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let incoming = source
        .readings
        .into_iter()
        .zip(source.reading_settings)
        .map(|(reading, settings)| {
            let provenance = format!(
                "journal:sha256={}:fingerprint={}:force=false:path={}",
                source.sha256,
                settings.fingerprint,
                args.from_journal.display()
            );
            (reading, settings, provenance)
        })
        .collect();
    let sink: &mut dyn ConflictSink = match &mut list {
        Some(list) => list,
        None => &mut NoConflictList,
    };
    let (appended, skipped, conflicts) =
        journal.merge_records(incoming, args.on_conflict, sink, &source.prefetched)?;
    println!("fingerprint={}", dst.fingerprint);
    println!("source_fingerprint={source_fingerprints}");
    println!("appended={appended} skipped={skipped} conflicts={conflicts}");
    Ok((appended, skipped, conflicts))
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
    use crate::book_tree::journal::{LockedFile, Reading};
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
            from_journal: source,
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
            from_journal: src_path.clone(),
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
            from_journal: source.clone(),
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
    fn filtered_out_positions_still_reject_conflicting_duplicates() {
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
        assert!(read_source(&path, None, keep).is_err());
    }

    #[test]
    fn every_selected_record_must_match_its_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let src = settings(&a, None);
        let path = dir.path().join("src.jsonl");
        let good = record_json(&src, &reading(0, &src, "resign"));

        let mut tampered = src.clone();
        tampered.nodes = 200;
        let lying = record_json(&tampered, &reading(1, &tampered, "resign"));
        write_lines(&path, &[good.clone(), lying.clone()]);
        assert!(read_source(&path, None, |_| true).is_err());
        write_lines(&path, &[lying]);
        assert!(read_source(&path, None, |_| true).is_err());

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
        assert!(read_source(&path, None, |_| true).is_err());

        // 選ばない条件の版の食い違いは問わない。
        let other = Settings::new(&reading_args(&a, 300, &[])).unwrap();
        let versioned = |v: &str| {
            let mut s = other.clone();
            s.engine_version = Some(v.into());
            record_json(&s, &reading(2, &s, "resign"))
        };
        write_lines(&path, &[good.clone(), versioned(VERSION_A), versioned(VERSION_B)]);
        assert_eq!(read_source(&path, Some(&src.fingerprint), |_| true).unwrap().readings.len(), 1);
        assert!(read_source(&path, Some(&other.fingerprint), |_| true).is_err());

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
            from_journal: src_path.clone(),
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
}
