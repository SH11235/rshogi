//! SFEN重複排除ユーティリティ

use crate::common::sfen::normalize_4t;
use crate::common::sfen_ops::canonicalize_4t_with_mirror;
use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

/// `--input` (カンマ区切り) または `--input-dir` + `--pattern` からファイル一覧を収集する。
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
        (Some(data), None) => {
            let paths: Vec<PathBuf> = data.split(',').map(|s| PathBuf::from(s.trim())).collect();
            for p in &paths {
                if !p.exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("入力ファイルが存在しません: {}", p.display()),
                    ));
                }
            }
            Ok(paths)
        }
        (None, Some(dir)) => {
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
                .filter(|e| {
                    e.path().file_name().and_then(|n| n.to_str()).is_some_and(|n| pat.matches(n))
                })
                .map(|e| e.into_path())
                .collect();
            paths.sort();
            Ok(paths)
        }
    }
}

/// まだ存在しないかもしれないパスを正規化する。
/// `parent().canonicalize() / file_name()` で解決する。
fn canonicalize_maybe_new(path: &Path) -> io::Result<PathBuf> {
    if let Ok(c) = path.canonicalize() {
        return Ok(c);
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "出力パスにファイル名がありません")
    })?;
    Ok(parent.canonicalize()?.join(name))
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

/// `statvfs(2)` で指定パスが存在するファイルシステムの空き容量（バイト）を取得する。
/// 取得できない場合は None。
pub fn get_disk_available(path: &Path) -> Option<u64> {
    let probe: &Path = if path.exists() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    let c = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs は POSIX 標準で、c は有効な C 文字列ポインタ、stat は書き込み可能な
    // 構造体を指す。失敗時は -1 を返すだけで副作用はない。
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some(stat.f_bavail * stat.f_frsize)
}

/// バイト値を `%.1f GiB` 形式で整形する。
pub fn format_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// 2 つのパスが同一ファイルシステム上にあるかを判定する。
/// 取得できない場合は None。
pub fn same_filesystem(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let probe_a: &Path = if a.exists() {
        a
    } else {
        a.parent().unwrap_or(Path::new("."))
    };
    let probe_b: &Path = if b.exists() {
        b
    } else {
        b.parent().unwrap_or(Path::new("."))
    };
    let ma = std::fs::metadata(probe_a).ok()?;
    let mb = std::fs::metadata(probe_b).ok()?;
    Some(ma.dev() == mb.dev())
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
