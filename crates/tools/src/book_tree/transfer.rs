//! 別の台帳 (別マシンの台帳の複製など) の読みを、取り込み先の条件で台帳に足す。

use super::ReadingArgs;
use super::journal::{Journal, Settings, read_source};
use anyhow::{Result, bail};
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
    /// 取り込み元の条件の指紋。省略時は取り込み元の唯一の条件
    #[arg(long)]
    fingerprint: Option<String>,
    /// 取り込み先の台帳
    #[arg(long)]
    journal: PathBuf,
    /// 取り込み先の読み条件 (serve と同じ指定)
    #[command(flatten)]
    reading: ReadingArgs,
    /// エンジンの版が違う・分からない読みも取り込む (探索の結果を変えない変更だけの版どうしで使う)
    #[arg(long)]
    force: bool,
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
            "エンジンが --source-version に答えないので版が分かりません。この条件の読みを別のバイナリとの間で取り込むには --force が要ります",
        ),
        Some(v) if v.starts_with("unknown") => Some(
            "エンジンの版が unknown です (git の作業木の外でのビルド、または git が使えない)。この条件の読みを別のバイナリとの間で取り込むには --force が要ります",
        ),
        Some(v) if v.contains("-dirty") => Some(
            "エンジンは commit されていない変更を含むビルド (-dirty) です。この条件の読みを別のバイナリとの間で取り込むには --force が要ります",
        ),
        Some(_) => None,
    }
}

fn parse_source_version(stdout: &str) -> Option<String> {
    let version = stdout.lines().find_map(|line| line.trim().strip_prefix("source-version "))?;
    let version = version.trim();
    (!version.is_empty() && !version.contains(char::is_whitespace)).then(|| version.to_owned())
}

/// commit が分かり、commit されていない変更を含まない版。
fn known_version(settings: &Settings) -> Option<&str> {
    settings
        .engine_version
        .as_deref()
        .filter(|v| !v.starts_with("unknown") && !v.contains("-dirty"))
}

/// 取り込み元の読みを取り込み先の条件の読みとみなせるかを確かめる。版だけが違う・分からないときは
/// `force` で許し、その理由を返す。
fn check_compatible(src: &Settings, dst: &Settings, force: bool) -> Result<Option<String>> {
    let mut errors = Vec::new();
    for (name, a, b) in [
        ("nodes", src.nodes.to_string(), dst.nodes.to_string()),
        ("multipv", src.multipv.to_string(), dst.multipv.to_string()),
        ("hash", src.hash.to_string(), dst.hash.to_string()),
    ] {
        if a != b {
            errors.push(format!("{name}: {a} / {b}"));
        }
    }
    // ファイル型の option はマシンごとにパスが違うので内容の SHA-256 で比べる。
    if src.content_sha256 != dst.content_sha256 {
        errors.push(format!("ファイルの内容: {:?} / {:?}", src.content_sha256, dst.content_sha256));
    }
    let plain = |s: &Settings| {
        s.options
            .iter()
            .filter(|(k, _)| !s.content_sha256.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
    };
    let src_keys: Vec<_> = src.options.keys().collect();
    let dst_keys: Vec<_> = dst.options.keys().collect();
    if src_keys != dst_keys || plain(src) != plain(dst) {
        errors.push(format!("option: {:?} / {:?}", plain(src), plain(dst)));
    }
    if !errors.is_empty() {
        bail!("取り込み元と取り込み先の条件が違います (元 / 先): {}", errors.join("; "));
    }
    // 同じバイナリなら版を問わない。違うバイナリは同じソースの版のときだけ同じ条件とする。
    if src.engine_sha256 == dst.engine_sha256 {
        return Ok(None);
    }
    match (known_version(src), known_version(dst)) {
        (Some(a), Some(b)) if a == b => Ok(None),
        _ => {
            let reason = format!(
                "エンジンの版: {} / {}",
                src.engine_version.as_deref().unwrap_or("不明"),
                dst.engine_version.as_deref().unwrap_or("不明")
            );
            if force {
                Ok(Some(reason))
            } else {
                bail!(
                    "{reason} (同じ版と確かめられません。探索の結果を変えない違いなら --force で取り込めます)"
                )
            }
        }
    }
}

pub(super) fn run(args: ImportJournalArgs) -> Result<()> {
    let mut dst = Settings::new(&args.reading)?;
    dst.engine_version = engine_source_version(&args.reading.engine);
    warn_unknown_version(&args.reading.engine, &dst.engine_version);
    let source = read_source(&args.from_journal, args.fingerprint.as_deref())?;
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
    let forced = check_compatible(&source.settings, &dst, args.force)?;
    if let Some(reason) = &forced {
        eprintln!("--force: {reason}");
    }
    let provenance = format!(
        "journal:sha256={}:fingerprint={}:force={}:path={}",
        source.sha256,
        source.settings.fingerprint,
        forced.is_some(),
        args.from_journal.display()
    );
    let mut journal = Journal::open(&args.journal, &dst.fingerprint)?;
    let incoming = source.readings.into_iter().map(|r| (r, provenance.clone())).collect();
    let (appended, skipped) = journal.append_all(incoming, &dst)?;
    println!("fingerprint={}", dst.fingerprint);
    println!("source_fingerprint={}", source.settings.fingerprint);
    println!("appended={appended} skipped={skipped}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_tree::journal::{LockedFile, Reading};
    use std::collections::BTreeMap;

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
    fn same_contents_settings_and_version_are_compatible_across_paths_and_builds() {
        let dir = tempfile::tempdir().unwrap();
        let src = settings(&machine(dir.path(), "a", "build a", "model"), Some(VERSION_A));
        let dst = settings(&machine(dir.path(), "b", "build b", "model"), Some(VERSION_A));
        assert_ne!(src.fingerprint, dst.fingerprint);
        assert_eq!(check_compatible(&src, &dst, false).unwrap(), None);
    }

    #[test]
    fn different_unknown_or_dirty_version_needs_force() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let b = machine(dir.path(), "b", "build b", "model");
        let dirty = format!("{VERSION_A}-dirty");
        for (src, dst) in [
            (Some(VERSION_A), Some(VERSION_B)),
            (None, Some(VERSION_A)),
            (Some(VERSION_A), Some("unknown")),
            (Some(dirty.as_str()), Some(dirty.as_str())),
        ] {
            let (src, dst) = (settings(&a, src), settings(&b, dst));
            assert!(check_compatible(&src, &dst, false).is_err());
            assert!(check_compatible(&src, &dst, true).unwrap().is_some());
        }
        // 同じバイナリなら版を記録する前の読みでも同じ条件。
        let same = settings(&a, None);
        assert_eq!(check_compatible(&same, &settings(&a, Some(VERSION_A)), false).unwrap(), None);
    }

    #[test]
    fn other_differences_are_refused_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine(dir.path(), "a", "build a", "model");
        let b = machine(dir.path(), "b", "build b", "other model");
        let c = machine(dir.path(), "c", "build c", "model");
        let src = settings(&a, Some(VERSION_A));
        let with = |m: &Machine, nodes, extra: &[&str]| {
            let mut s = Settings::new(&reading_args(m, nodes, extra)).unwrap();
            s.engine_version = Some(VERSION_A.into());
            s
        };
        for dst in [
            settings(&b, Some(VERSION_A)),
            with(&c, 200, &["DrawValueBlack=-50"]),
            with(&c, 100, &["DrawValueBlack=0"]),
            with(&c, 100, &["DrawValueBlack=-50", "SlowMover=50"]),
        ] {
            assert!(check_compatible(&src, &dst, true).is_err());
        }
        std::fs::write(c.engine.with_file_name("spsa.params"), "tuned").unwrap();
        assert!(check_compatible(&src, &settings(&c, Some(VERSION_A)), true).is_err());
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

        assert!(read_source(&src_path, None).is_err());
        let source = read_source(&src_path, Some(&src.fingerprint)).unwrap();
        assert!(source.ignored_tail);
        assert_eq!(source.readings.len(), 2);
        assert_eq!(source.settings.engine_version.as_deref(), Some(VERSION_A));
        assert_eq!(std::fs::read(&src_path).unwrap(), bytes);
        {
            let _writer = LockedFile::open(&src_path).unwrap();
            assert!(read_source(&src_path, Some(&src.fingerprint)).is_err());
        }

        let dst = settings(&b, Some(VERSION_A));
        let dst_path = dir.path().join("dst.jsonl");
        write_journal(&dst_path, &[(&dst, reading(0, &dst, "resign"))]);
        let mut journal = Journal::open(&dst_path, &dst.fingerprint).unwrap();
        let incoming: Vec<_> =
            source.readings.iter().map(|r| (r.clone(), "copied".to_owned())).collect();
        assert_eq!(journal.append_all(incoming, &dst).unwrap(), (1, 1));
        drop(journal);

        let mut conflicting = reading(3, &dst, "resign");
        let size = std::fs::metadata(&dst_path).unwrap().len();
        let mut journal = Journal::open(&dst_path, &dst.fingerprint).unwrap();
        let mut changed = reading(0, &dst, "resign");
        changed.bestmove = "win".into();
        conflicting.tag = "new".into();
        let incoming = vec![(conflicting, "copied".into()), (changed, "copied".into())];
        assert!(journal.append_all(incoming, &dst).is_err());
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

    /// serve が書いた台帳の条件 (ファイルのパスだけ置き換えたもの) と、そのとき記録された指紋。
    const RECORDED_SETTINGS: &str = r#"{"fingerprint":"74695922da474f8e2284c42755db2d93b0fb80815b63a253a1ae071c6ac8942a","version":"book-tree-v1/newgame-ready/node-calib-last-multipv-v1","engine_sha256":"8c32f9585e25eb773b008e05fcd03cd7c6e590660b5eda0a561e3746ce877984","options":{"DrawValueBlack":"-70","DrawValueWhite":"70","EvalFile":"/path/to/model.bin","FV_SCALE":"14","LS_BUCKET_MODE":"progresskpabs","LS_PROGRESS_BUCKETS":"8","LS_PROGRESS_COEFF":"/path/to/progress.bin","MultiPV":"3","Threads":"1","USI_Hash":"1024","UseEvalHash":"false"},"content_sha256":{"EvalFile":"7871f1c0b7af35d84e76a713b20e7b313f8294167d513761f0a69b4e24c4003d","LS_PROGRESS_COEFF":"8d0058b899768c00e71be5508c5c4544d8c8ce24b30c32024c2a0a4452dbdb53","implicit:SPSAParamsFile":"absent"},"nodes":1000000000,"multipv":3,"hash":1024}"#;

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
        assert!(read_source(&path, None).is_err());
        write_lines(&path, &[lying]);
        assert!(read_source(&path, None).is_err());

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
        assert_eq!(read_source(&path, None).unwrap().readings.len(), 2);
        let mut changed_draw = src.clone();
        changed_draw.options.insert("DrawValueBlack".into(), "0".into());
        write_lines(
            &path,
            &[
                good.clone(),
                record_json(&changed_draw, &reading(3, &src, "resign")),
            ],
        );
        assert!(read_source(&path, None).is_err());

        // 選ばない条件の版の食い違いは問わない。
        let other = Settings::new(&reading_args(&a, 300, &[])).unwrap();
        let versioned = |v: &str| {
            let mut s = other.clone();
            s.engine_version = Some(v.into());
            record_json(&s, &reading(2, &s, "resign"))
        };
        write_lines(&path, &[good.clone(), versioned(VERSION_A), versioned(VERSION_B)]);
        assert_eq!(read_source(&path, Some(&src.fingerprint)).unwrap().readings.len(), 1);
        assert!(read_source(&path, Some(&other.fingerprint)).is_err());

        std::fs::write(&path, "").unwrap();
        let err = read_source(&path, None).err().unwrap().to_string();
        assert!(err.contains("取り込み元の台帳が空です"), "{err}");
        let missing = dir.path().join("missing.jsonl");
        let err = format!("{:#}", read_source(&missing, None).err().unwrap());
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
    fn import_journal_records_provenance_and_requires_force_for_another_version() {
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
            journal: dst_path.clone(),
            reading: reading_args(&b, 100, &[]),
            force,
        };
        assert!(run(args(false)).is_err());
        assert!(!dst_path.exists() || std::fs::metadata(&dst_path).unwrap().len() == 0);
        run(args(true)).unwrap();
        let line = std::fs::read_to_string(&dst_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let sha = super::super::journal::digest(&src_path).unwrap();
        assert_eq!(
            record["provenance"],
            format!(
                "journal:sha256={sha}:fingerprint={}:force=true:path={}",
                src.fingerprint,
                src_path.display()
            )
        );
        assert_eq!(record["settings"]["engine_version"], VERSION_B);
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
