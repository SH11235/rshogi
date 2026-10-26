//! 定跡の探索 journal を再利用するための入力内容の識別。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// 実行ファイルを解決する。絶対化で Windows の別候補が優先される場合は元の起動名を保つ。
pub fn resolve_engine_path(engine: &Path) -> Result<PathBuf> {
    let selected = selected_engine_path(engine)?;
    #[cfg(windows)]
    if !explicit_engine_name(engine) && explicit_engine_path(&selected) != selected {
        // 非 .exe の bare 名は、絶対化すると sibling の .exe を std が優先する場合がある。
        return Ok(engine.to_owned());
    }
    Ok(selected)
}

fn explicit_engine_name(engine: &Path) -> bool {
    let encoded = engine.as_os_str().as_encoded_bytes();
    #[cfg(windows)]
    {
        encoded.iter().any(|byte| matches!(byte, b'/' | b'\\' | b':'))
    }
    #[cfg(not(windows))]
    {
        encoded.contains(&b'/')
    }
}

fn selected_engine_path(engine: &Path) -> Result<PathBuf> {
    let encoded = engine.as_os_str().as_encoded_bytes();
    anyhow::ensure!(!encoded.is_empty(), "engine executable name is empty");
    let selected = if explicit_engine_name(engine) {
        explicit_engine_path(engine)
    } else {
        let directories = executable_search_directories(std::env::var_os("PATH"));
        search_engine_path(engine, directories)
            .with_context(|| format!("engine executable not found: {}", engine.display()))?
    };
    std::path::absolute(&selected)
        .with_context(|| format!("engine executable path: {}", selected.display()))
}

#[cfg(not(windows))]
fn explicit_engine_path(engine: &Path) -> PathBuf {
    engine.to_owned()
}

#[cfg(windows)]
fn explicit_engine_path(engine: &Path) -> PathBuf {
    let encoded = engine.as_os_str().as_encoded_bytes();
    if encoded.len() >= 4 && encoded[encoded.len() - 4..].eq_ignore_ascii_case(b".exe") {
        return engine.to_owned();
    }
    let mut appended = engine.as_os_str().to_owned();
    appended.push(".exe");
    let appended = PathBuf::from(appended);
    if appended.symlink_metadata().is_ok() {
        appended
    } else {
        engine.to_owned()
    }
}

fn executable_search_directories(paths: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(executable) = std::env::current_exe()
            && let Some(parent) = executable.parent()
        {
            directories.push(parent.to_owned());
        }
        for folder in [
            known_folders::KnownFolder::System,
            known_folders::KnownFolder::Windows,
        ] {
            if let Some(path) = known_folders::get_known_folder_path(folder) {
                directories.push(path);
            }
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    let paths = paths.or_else(|| Some("/bin:/usr/bin".into()));
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    let paths = paths.or_else(|| Some("/usr/local/bin:/bin:/usr/bin".into()));
    if let Some(paths) = paths {
        #[cfg(windows)]
        directories
            .extend(std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()));
        #[cfg(not(windows))]
        directories.extend(std::env::split_paths(&paths));
    }
    directories
}

fn search_engine_path(engine: &Path, directories: Vec<PathBuf>) -> Option<PathBuf> {
    for directory in directories {
        let candidate = directory.join(engine);
        #[cfg(windows)]
        let candidate = {
            let mut candidate = candidate;
            if !engine.as_os_str().as_encoded_bytes().contains(&b'.') {
                candidate.set_extension("exe");
            }
            candidate
        };
        #[cfg(windows)]
        let executable = candidate.symlink_metadata().is_ok();
        #[cfg(unix)]
        let executable = candidate.is_file()
            && rustix::fs::accessat(
                rustix::fs::CWD,
                &candidate,
                rustix::fs::Access::EXEC_OK,
                rustix::fs::AtFlags::EACCESS,
            )
            .is_ok();
        #[cfg(not(any(unix, windows)))]
        let executable = candidate.is_file();
        if executable {
            return Some(candidate);
        }
    }
    None
}

fn file_option(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["file", "dir", "path", "coeff"].iter().any(|suffix| lower.ends_with(suffix))
}

fn options(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    let mut names = BTreeSet::new();
    for option in values {
        let (name, value) = option.split_once('=').unwrap_or((option, ""));
        let name = name.trim();
        let value = value.trim();
        if name.is_empty()
            || name.contains(['\r', '\n'])
            || value.contains(['\r', '\n'])
            || !names.insert(name.to_ascii_lowercase())
        {
            bail!("重複または不正な engine-option: {option}");
        }
        parsed.insert(name.to_owned(), value.to_owned());
    }
    Ok(parsed)
}

/// ファイルとディレクトリの内容を、名前順・有界読み込みで識別する。
pub fn content_digest(path: &Path) -> Result<String> {
    digest(path, &mut BTreeSet::new())
}

fn digest(path: &Path, ancestors: &mut BTreeSet<PathBuf>) -> Result<String> {
    let resolved =
        path.canonicalize().with_context(|| format!("内容の指紋: {}", path.display()))?;
    let mut hash = Sha256::new();
    if resolved.is_dir() {
        if !ancestors.insert(resolved.clone()) {
            bail!("ディレクトリのリンクが循環しています: {}", path.display());
        }
        hash.update(b"directory");
        let mut entries = std::fs::read_dir(&resolved)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().context("指紋のファイル名は UTF-8 が必要です")?;
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update(digest(&entry.path(), ancestors)?.as_bytes());
        }
        ancestors.remove(&resolved);
    } else {
        hash.update(b"file");
        let mut file = File::open(&resolved)?;
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// 明示したモデル資源があり、既定モデルの推測なしに再利用できるかを返す。
pub fn has_explicit_model(values: &[String]) -> Result<bool> {
    Ok(options(values)?.iter().any(|(name, value)| {
        (name.eq_ignore_ascii_case("EvalFile") || name.eq_ignore_ascii_case("EvalDir"))
            && !matches!(value.as_str(), "" | "none" | "<empty>")
    }))
}

struct Resources {
    paths: Vec<PathBuf>,
    reusable: bool,
}

impl Resources {
    fn add(&mut self, path: PathBuf, stable: bool) {
        self.reusable &= stable;
        match path.try_exists() {
            Ok(true) => self.paths.push(path),
            _ => self.reusable = false,
        }
    }
}

fn resolved_resources(engine: &Path, parsed: &BTreeMap<String, String>) -> Resources {
    let selected = selected_engine_path(engine).unwrap_or_else(|_| engine.to_owned());
    let engine = selected.as_path();
    let mut resources = Resources {
        paths: vec![engine.to_owned()],
        reusable: false,
    };
    let eval_dir = parsed.get("EvalDir").map(PathBuf::from);
    let book_file = parsed.get("BookFile");
    for (name, value) in parsed {
        match name.as_str() {
            "EvalFile" | "EvalDir" => {
                if matches!(value.as_str(), "" | "<empty>" | "<auto>" | "none") {
                    continue;
                }
                let path = PathBuf::from(value);
                if path.is_absolute() && path.try_exists().unwrap_or(false) {
                    resources.reusable = true;
                }
            }
            _ => {}
        }
    }
    for (name, value) in parsed {
        match name.as_str() {
            "EvalFile" => {
                if matches!(value.as_str(), "" | "<empty>" | "<auto>" | "none") {
                    continue;
                }
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    resources.add(path, true);
                } else {
                    // 相対 EvalFile の基準はエンジンごとに異なるため、再利用の根拠にはしない。
                    resources.reusable = false;
                    if path.try_exists().unwrap_or(false) {
                        resources.add(path, false);
                    }
                    if let Some(directory) = &eval_dir {
                        let combined = directory.join(value);
                        if combined.try_exists().unwrap_or(false) {
                            resources.add(combined, false);
                        }
                    }
                }
            }
            "EvalDir" => {
                if !matches!(value.as_str(), "" | "<empty>" | "<auto>" | "none") {
                    let path = PathBuf::from(value);
                    let stable = path.is_absolute();
                    resources.add(path, stable);
                }
            }
            "BookDir" => {
                if book_file.is_none() {
                    resources.reusable = false;
                    let path = PathBuf::from(if value.is_empty() { "book" } else { value });
                    if path.try_exists().unwrap_or(false) {
                        resources.add(path, false);
                    }
                }
            }
            "BookFile" => {
                if matches!(value.as_str(), "" | "no_book") {
                    continue;
                }
                let directory = parsed
                    .get("BookDir")
                    .map(String::as_str)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("book");
                let path = Path::new(directory).join(value);
                let stable = path.is_absolute();
                resources.add(path, stable);
            }
            "SPSAParamsFile" => {
                if matches!(value.as_str(), "" | "<empty>" | "<auto>") {
                    continue;
                }
                let path = PathBuf::from(value);
                let stable = path.is_absolute();
                resources.add(path, stable);
            }
            "LS_PROGRESS_COEFF" | "BookExploreFile" => {
                if matches!(value.as_str(), "" | "<empty>") {
                    continue;
                }
                let path = PathBuf::from(value);
                let stable = path.is_absolute();
                resources.add(path, stable);
            }
            _ if file_option(name) => {
                // 名前の接尾辞だけでは、読込資源か出力先かも判別できない。
                resources.reusable = false;
                let candidate = PathBuf::from(value);
                if candidate.try_exists().unwrap_or(false) {
                    resources.add(candidate, false);
                }
            }
            _ => {}
        }
    }
    let automatic_params = parsed
        .get("SPSAParamsFile")
        .is_none_or(|value| matches!(value.as_str(), "" | "<empty>" | "<auto>"));
    if automatic_params {
        match engine
            .canonicalize()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("spsa.params")))
        {
            Some(path) => match path.try_exists() {
                Ok(true) => resources.paths.push(path),
                Ok(false) => {}
                Err(_) => resources.reusable = false,
            },
            None => resources.reusable = false,
        }
    }
    resources.paths.sort();
    resources.paths.dedup();
    resources
}

/// 解決できた読込資源を返す。不確実な option の実在を fresh 起動の条件にしない。
pub fn readonly_resources(engine: &Path, values: &[String]) -> Result<Vec<PathBuf>> {
    Ok(resolved_resources(engine, &options(values)?).paths)
}

/// 読込資源の内容を識別する。解決を保証できない設定には実行固有の値を含める。
pub fn engine_fingerprint(engine: &Path, values: &[String]) -> Result<String> {
    let parsed = options(values)?;
    let mut resources = resolved_resources(engine, &parsed);
    let mut hashes = Vec::new();
    for path in &resources.paths {
        match content_digest(path) {
            Ok(hash) => hashes.push((path, hash)),
            Err(_) => resources.reusable = false,
        }
    }
    let nonce = if resources.reusable {
        None
    } else {
        eprintln!(
            "警告: 読込資源の解決を保証できないため、この実行では過去の journal を再利用しません"
        );
        Some(rand::random::<u128>())
    };
    Ok(format!(
        "content-v2:{:x}",
        Sha256::digest(serde_json::to_vec(&(parsed, hashes, nonce))?)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_executable_path_is_absolute_without_following_path_search() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join(format!("explicit{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&executable, b"binary").unwrap();
        assert_eq!(resolve_engine_path(&executable).unwrap(), executable);
    }

    #[test]
    #[cfg(windows)]
    fn windows_exe_completion_matches_bare_and_explicit_paths() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("test-engine.exe");
        std::fs::write(&executable, b"binary").unwrap();
        assert_eq!(
            search_engine_path(Path::new("test-engine"), vec![dir.path().to_owned()]).unwrap(),
            executable
        );
        assert_eq!(resolve_engine_path(&dir.path().join("test-engine")).unwrap(), executable);
        let extended = dir.path().join("test-engine.custom.exe");
        std::fs::write(&extended, b"binary").unwrap();
        assert!(
            search_engine_path(Path::new("test-engine.custom"), vec![dir.path().to_owned()])
                .is_none()
        );
        assert_eq!(resolve_engine_path(&dir.path().join("test-engine.custom")).unwrap(), extended);
    }

    #[test]
    #[cfg(unix)]
    fn unix_path_search_skips_non_executable_candidates() {
        use std::os::unix::fs::PermissionsExt;
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let name = "test-engine";
        for directory in [first.path(), second.path()] {
            std::fs::write(directory.join(name), b"binary").unwrap();
        }
        std::fs::set_permissions(first.path().join(name), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        std::fs::set_permissions(second.path().join(name), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(
            search_engine_path(
                Path::new(name),
                vec![first.path().to_owned(), second.path().to_owned()]
            )
            .unwrap(),
            second.path().join(name)
        );
    }

    #[test]
    fn same_path_changes_when_engine_model_or_implicit_params_change() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let model = dir.path().join("net.bin");
        std::fs::write(&engine, b"binary-a").unwrap();
        std::fs::write(&model, b"model-a").unwrap();
        let opts = vec![format!("EvalFile={}", model.display())];
        let before = engine_fingerprint(&engine, &opts).unwrap();
        assert_eq!(before, engine_fingerprint(&engine, &opts).unwrap());
        std::fs::write(&engine, b"binary-b").unwrap();
        let binary_changed = engine_fingerprint(&engine, &opts).unwrap();
        assert_ne!(before, binary_changed);
        std::fs::write(&model, b"model-b").unwrap();
        let model_changed = engine_fingerprint(&engine, &opts).unwrap();
        assert_ne!(binary_changed, model_changed);
        std::fs::write(dir.path().join("spsa.params"), b"params").unwrap();
        assert_ne!(model_changed, engine_fingerprint(&engine, &opts).unwrap());
    }

    #[test]
    fn directory_contents_are_hashed_and_option_order_is_irrelevant() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let models = dir.path().join("models");
        std::fs::write(&engine, b"binary").unwrap();
        std::fs::create_dir(&models).unwrap();
        std::fs::write(models.join("net.bin"), b"a").unwrap();
        let mut opts = vec![format!("EvalDir={}", models.display()), "Threads=1".into()];
        let before = engine_fingerprint(&engine, &opts).unwrap();
        opts.reverse();
        assert_eq!(before, engine_fingerprint(&engine, &opts).unwrap());
        std::fs::write(models.join("net.bin"), b"b").unwrap();
        assert_ne!(before, engine_fingerprint(&engine, &opts).unwrap());
    }

    #[test]
    fn automatic_params_sentinels_include_the_adjacent_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::write(&engine, b"binary").unwrap();
        let params = dir.path().join("spsa.params");
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"model").unwrap();
        std::fs::write(&params, b"params-a").unwrap();
        for value in ["", "<empty>", "<auto>"] {
            let opts = vec![
                format!("SPSAParamsFile={value}"),
                format!("EvalFile={}", model.display()),
            ];
            let before = engine_fingerprint(&engine, &opts).unwrap();
            std::fs::write(&params, b"params-b").unwrap();
            assert_ne!(before, engine_fingerprint(&engine, &opts).unwrap());
            std::fs::write(&params, b"params-a").unwrap();
        }
    }

    #[test]
    fn disabled_book_does_not_require_a_book_directory() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let model = dir.path().join("model.bin");
        std::fs::write(&engine, b"binary").unwrap();
        std::fs::write(&model, b"model").unwrap();
        let options = vec![
            format!("EvalFile={}", model.display()),
            "BookFile=no_book".into(),
            "BookDir=missing".into(),
            "BookExploreFile=<empty>".into(),
        ];
        let resources = readonly_resources(&engine, &options).unwrap();
        assert_eq!(resources.len(), 2);
        assert_eq!(
            engine_fingerprint(&engine, &options).unwrap(),
            engine_fingerprint(&engine, &options).unwrap()
        );
    }

    #[test]
    fn book_filename_is_resolved_under_book_dir_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let model = dir.path().join("model.bin");
        let books = dir.path().join("books");
        std::fs::create_dir(&books).unwrap();
        let book = books.join("opening.db");
        for path in [&engine, &model, &book] {
            std::fs::write(path, b"a").unwrap();
        }
        let options = vec![
            format!("EvalFile={}", model.display()),
            format!("BookDir={}", books.display()),
            "BookFile=opening.db".into(),
        ];
        assert!(readonly_resources(&engine, &options).unwrap().contains(&book));
        let before = engine_fingerprint(&engine, &options).unwrap();
        std::fs::write(&book, b"b").unwrap();
        assert_ne!(before, engine_fingerprint(&engine, &options).unwrap());
    }

    #[test]
    fn unresolved_and_arbitrary_file_options_disable_reuse_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let models = dir.path().join("models");
        std::fs::create_dir(&models).unwrap();
        std::fs::write(&engine, b"binary").unwrap();
        std::fs::write(models.join("model.bin"), b"model").unwrap();
        for extra in ["EvalFile=model.bin", "CustomFile=false"] {
            let options = vec![format!("EvalDir={}", models.display()), extra.into()];
            assert!(readonly_resources(&engine, &options).is_ok());
            assert_ne!(
                engine_fingerprint(&engine, &options).unwrap(),
                engine_fingerprint(&engine, &options).unwrap()
            );
        }
    }

    #[test]
    fn missing_model_disables_reuse_and_duplicates_fail() {
        assert!(!has_explicit_model(&[]).unwrap());
        assert!(!has_explicit_model(&["EvalFile=none".into()]).unwrap());
        assert!(has_explicit_model(&["EvalFile=model.bin".into()]).unwrap());
        assert!(has_explicit_model(&["EvalFile=a".into(), "evalfile=b".into()]).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn directory_symlink_cycle_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        assert!(content_digest(dir.path()).is_err());
    }
}
