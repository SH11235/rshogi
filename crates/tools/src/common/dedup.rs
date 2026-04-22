//! SFEN重複排除ユーティリティ

use crate::common::sfen::normalize_4t;
use crate::common::sfen_ops::canonicalize_4t_with_mirror;
use std::collections::HashSet;
use std::io;
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
    fn test_dedup_with_mirror() {
        let a = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/2B4R1/LNSGKGSNL b - 1";
        let b = "lnsgkgsnl/1b5r1/ppppppppp/9/9/9/PPPPPPPPP/1R4B2/LNSGKGSNL b - 1";
        let mut d = DedupSet::new(true);
        assert!(d.insert(a));
        assert!(!d.insert(b));
        assert_eq!(d.len(), 1);
    }
}
