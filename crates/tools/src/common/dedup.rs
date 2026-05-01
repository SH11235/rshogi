//! SFEN重複排除ユーティリティ

use crate::common::sfen::normalize_4t;
use crate::common::sfen_ops::canonicalize_4t_with_mirror;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::path::{Component, Prefix};
#[cfg(not(unix))]
use sysinfo::Disks;

/// PackedSfenValue のレコードサイズ（バイト）
pub const PSV_SIZE: usize = 40;
/// PackedSfen 部分のサイズ（バイト）
pub const SFEN_SIZE: usize = 32;

/// PackedSfen（32バイト）の FNV-1a 64bit ハッシュ
pub fn hash_packed_sfen(sfen: &[u8; SFEN_SIZE]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in sfen.iter() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// PSV レコードから game_ply を取得（offset 36, u16 LE）
pub fn game_ply_from_record(record: &[u8; PSV_SIZE]) -> u16 {
    u16::from_le_bytes([record[36], record[37]])
}

/// `--input` (カンマ区切りのファイル / ディレクトリ / glob) または
/// `--input-dir` + `--pattern` からファイル一覧を収集する。
pub fn collect_input_paths(
    input: Option<&str>,
    input_dir: Option<&PathBuf>,
    pattern: &str,
) -> io::Result<Vec<PathBuf>> {
    match (input, input_dir) {
        (Some(_), Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--data/--input と --input-dir は同時に指定できません",
        )),
        (None, None) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--data/--input または --input-dir のいずれかを指定してください",
        )),
        (Some(data), None) => collect_input_specs(data, pattern),
        (None, Some(dir)) => collect_input_dir(dir, pattern),
    }
}

fn collect_input_specs(input: &str, pattern: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for spec in input.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(spec);
        if path.is_file() {
            paths.push(path);
        } else if path.is_dir() {
            paths.extend(collect_input_dir(&path, pattern)?);
        } else if has_glob_metachar(spec) {
            paths.extend(collect_input_glob(spec)?);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("入力ファイルまたはディレクトリが存在しません: {}", path.display()),
            ));
        }
    }

    if paths.is_empty() && !input.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("入力に一致するファイルが見つかりません: {input}"),
        ));
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_input_dir(dir: &Path, pattern: &str) -> io::Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("入力ディレクトリが存在しません: {}", dir.display()),
        ));
    }
    let pat = glob::Pattern::new(pattern).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("無効な glob パターン: {e}"))
    })?;
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().file_name().and_then(|n| n.to_str()).is_some_and(|n| pat.matches(n)))
        .map(|e| e.into_path())
        .collect();
    paths.sort();
    Ok(paths)
}

fn collect_input_glob(pattern: &str) -> io::Result<Vec<PathBuf>> {
    let entries = glob::glob(pattern).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("無効な glob パターン: {e}"))
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| io::Error::other(format!("glob 展開に失敗しました: {e}")))?;
        if path.is_file() {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("glob に一致する入力ファイルが見つかりません: {pattern}"),
        ));
    }
    paths.sort();
    Ok(paths)
}

fn has_glob_metachar(spec: &str) -> bool {
    spec.contains('*') || spec.contains('?') || spec.contains('[')
}

/// まだ存在しないかもしれないパスを正規化する。
///
/// 既存の最長祖先ディレクトリだけを `canonicalize` し、残りの成分は
/// パス構文どおりに連結する。これにより、まだ存在しない出力ファイルや
/// 親ディレクトリを含むパスでも比較用の絶対パスへ正規化できる。
pub fn canonicalize_maybe_new(path: &Path) -> io::Result<PathBuf> {
    let components: Vec<_> = path.components().collect();

    for prefix_len in (0..=components.len()).rev() {
        let prefix = join_components(&components[..prefix_len]);
        let base = if prefix_len == 0 && path.is_relative() {
            std::env::current_dir()?.canonicalize()?
        } else if prefix.as_os_str().is_empty() {
            continue;
        } else if let Ok(base) = prefix.canonicalize() {
            base
        } else {
            continue;
        };

        return Ok(append_components(base, &components[prefix_len..]));
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("パスを正規化できませんでした: {}", path.display()),
    ))
}

fn join_components(components: &[std::path::Component<'_>]) -> PathBuf {
    let mut path = PathBuf::new();
    for component in components {
        path.push(component.as_os_str());
    }
    path
}

fn append_components(mut base: PathBuf, components: &[std::path::Component<'_>]) -> PathBuf {
    for component in components {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = base.pop();
            }
            std::path::Component::Normal(part) => base.push(part),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                base.push(component.as_os_str());
            }
        }
    }
    base
}

/// 出力パスが入力パスのいずれかと一致していないか検査する。
/// 一致していればエラーを返す。出力ファイルがまだ存在しない場合でも正しく検出する。
pub fn check_output_not_in_inputs(output: &Path, inputs: &[PathBuf]) -> io::Result<()> {
    let out_canonical = canonicalize_maybe_new(output)?;
    for p in inputs {
        if let Ok(ic) = p.canonicalize()
            && ic == out_canonical
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("出力ファイルが入力ファイルと同一です: {}", p.display()),
            ));
        }
    }
    Ok(())
}

/// 入力ファイル群のサイズ合計（バイト）を返す。
pub fn sum_file_sizes(paths: &[PathBuf]) -> io::Result<u64> {
    let mut total = 0u64;
    for p in paths {
        total += std::fs::metadata(p)?.len();
    }
    Ok(total)
}

/// /proc/meminfo から MemAvailable をバイト単位で取得する。
/// 取得できない環境では None を返す。
pub fn get_mem_available() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb_str = rest.trim().strip_suffix("kB")?.trim();
            let kb: u64 = kb_str.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn disk_probe_path(path: &Path) -> Option<PathBuf> {
    let probe = if path.exists() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    canonicalize_maybe_new(probe).ok()
}

#[cfg(windows)]
fn normalize_mount_compare_path(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return PathBuf::new();
    };

    let mut normalized = PathBuf::new();
    match first {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => normalized.push(format!("{}:", drive as char)),
            _ => normalized.push(first.as_os_str()),
        },
        _ => normalized.push(first.as_os_str()),
    }

    for component in components {
        normalized.push(component.as_os_str());
    }
    normalized
}

#[cfg(all(not(unix), not(windows)))]
fn normalize_mount_compare_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(not(unix))]
fn disk_for_path(path: &Path) -> Option<(PathBuf, u64)> {
    let probe = normalize_mount_compare_path(&disk_probe_path(path)?);
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter_map(|disk| {
            let mount = normalize_mount_compare_path(disk.mount_point());
            probe.starts_with(&mount).then_some((mount, disk.available_space()))
        })
        .max_by_key(|(mount, _)| mount.components().count())
}

/// 指定パスが属するファイルシステムの空き容量（バイト）を取得する。
/// 取得できない場合は None。
#[cfg(unix)]
pub fn get_disk_available(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let probe = disk_probe_path(path)?;
    let c = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs は POSIX 標準で、c は NUL を含まない有効な C 文字列、
    // stat は書き込み可能な領域を指す。失敗時は None を返すだけ。
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some(stat.f_bavail * stat.f_frsize)
}

/// 指定パスが属するファイルシステムの空き容量（バイト）を取得する。
/// 取得できない場合は None。
#[cfg(not(unix))]
pub fn get_disk_available(path: &Path) -> Option<u64> {
    disk_for_path(path).map(|(_, available_space)| available_space)
}

/// バイト値を `%.1f GiB` 形式で整形する。
pub fn format_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// 2 つのパスが同一ファイルシステム上にあるかを判定する。
/// 取得できない場合は None。
#[cfg(unix)]
pub fn same_filesystem(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;

    let probe_a = disk_probe_path(a)?;
    let probe_b = disk_probe_path(b)?;
    let ma = std::fs::metadata(probe_a).ok()?;
    let mb = std::fs::metadata(probe_b).ok()?;
    Some(ma.dev() == mb.dev())
}

/// 2 つのパスが同一ファイルシステム上にあるかを判定する。
/// 取得できない場合は None。
#[cfg(not(unix))]
pub fn same_filesystem(a: &Path, b: &Path) -> Option<bool> {
    let (mount_a, _) = disk_for_path(a)?;
    let (mount_b, _) = disk_for_path(b)?;
    Some(mount_a == mount_b)
}

/// In-memory de-duplicator keyed by 4-token SFEN or mirror-canonicalized 4-token SFEN.
pub struct DedupSet {
    set: HashSet<String>,
    canonical_with_mirror: bool,
}

impl DedupSet {
    pub fn new(canonical_with_mirror: bool) -> Self {
        Self {
            set: HashSet::new(),
            canonical_with_mirror,
        }
    }

    pub fn insert(&mut self, sfen: &str) -> bool {
        let key = if self.canonical_with_mirror {
            canonicalize_4t_with_mirror(sfen)
        } else {
            normalize_4t(sfen)
        };
        if let Some(k) = key {
            self.set.insert(k)
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn canonicalize_maybe_new_handles_new_relative_file_in_cwd() {
        let expected =
            std::env::current_dir().unwrap().canonicalize().unwrap().join("train_000.bin");

        assert_eq!(canonicalize_maybe_new(Path::new("train_000.bin")).unwrap(), expected);
    }

    #[test]
    fn canonicalize_maybe_new_handles_nonexistent_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out/merged.psv");

        assert_eq!(
            canonicalize_maybe_new(&output).unwrap(),
            dir.path().canonicalize().unwrap().join("out/merged.psv")
        );
    }

    #[test]
    fn check_output_not_in_inputs_accepts_new_output_under_new_parent() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        fs::write(&input, [0u8; PSV_SIZE]).unwrap();

        check_output_not_in_inputs(&dir.path().join("new/out.psv"), &[input]).unwrap();
    }

    #[test]
    fn collect_input_paths_accepts_glob_in_input() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let ignored = dir.path().join("c.txt");
        fs::write(&a, [0u8; PSV_SIZE]).unwrap();
        fs::write(&b, [1u8; PSV_SIZE]).unwrap();
        fs::write(ignored, []).unwrap();

        let pattern = format!("{}/*.bin", dir.path().display());
        let paths = collect_input_paths(Some(&pattern), None, "*.bin").unwrap();

        assert_eq!(paths, vec![a, b]);
    }

    #[test]
    fn collect_input_paths_accepts_directory_in_input() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("nested/b.bin");
        fs::create_dir_all(b.parent().unwrap()).unwrap();
        fs::write(&a, [0u8; PSV_SIZE]).unwrap();
        fs::write(&b, [1u8; PSV_SIZE]).unwrap();

        let input = dir.path().to_string_lossy();
        let paths = collect_input_paths(Some(&input), None, "*.bin").unwrap();

        assert_eq!(paths, vec![a, b]);
    }

    #[test]
    fn disk_probe_path_uses_existing_parent_for_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let probe = disk_probe_path(&dir.path().join("out/new.psv")).unwrap();

        assert_eq!(probe, dir.path().canonicalize().unwrap().join("out"));
    }

    #[cfg(unix)]
    #[test]
    fn same_filesystem_returns_true_within_same_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.psv");
        let b = dir.path().join("nested/b.psv");
        fs::create_dir_all(b.parent().unwrap()).unwrap();

        assert_eq!(same_filesystem(&a, &b), Some(true));
    }

    #[cfg(not(unix))]
    #[test]
    fn normalize_mount_compare_path_normalizes_verbatim_disk_prefix() {
        let original = Path::new(r"\\?\C:\work\tmp\dedup");
        let normalized = normalize_mount_compare_path(original);

        assert_eq!(normalized, PathBuf::from(r"C:\work\tmp\dedup"));
    }

    #[test]
    fn test_dedup_with_mirror() {
        let a = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/2B4R1/LNSGKGSNL b - 1";
        let b = "lnsgkgsnl/1b5r1/ppppppppp/9/9/9/PPPPPPPPP/1R4B2/LNSGKGSNL b - 1";
        let mut d = DedupSet::new(true);
        assert!(d.insert(a));
        assert!(!d.insert(b));
        assert_eq!(d.len(), 1);
    }
}
