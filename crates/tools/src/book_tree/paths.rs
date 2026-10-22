use super::model::Definition;
use crate::common::dedup::canonicalize_maybe_new;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub(super) fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("管理パスにシンボリックリンクは使用できません: {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn normalized(path: &Path) -> Result<PathBuf> {
    let path = canonicalize_maybe_new(path)?;
    #[cfg(windows)]
    let path = PathBuf::from(path.as_os_str().to_string_lossy().to_lowercase());
    Ok(path)
}

fn managed(path: &Path) -> Result<PathBuf> {
    reject_symlink(path)?;
    normalized(path)
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub(super) fn reject_collisions(
    journal: &Path,
    state: Option<&Path>,
    definitions: &[&Definition],
) -> Result<()> {
    let journal = managed(journal)?;
    let mut protected = vec![journal.clone()];
    if let Some(state) = state {
        let state = managed(state)?;
        for name in [
            super::targets::QUEUE,
            super::targets::QUEUE_LOCK,
            super::targets::STATUS_FILE,
            super::targets::RUN_DIR,
            "restart",
            "lock",
            "workers",
            "stop",
            "STATUS",
            "ALERT",
            "progress.json",
            super::targets::CHECKPOINT,
        ] {
            let path = managed(&state.join(name))?;
            if overlap(&journal, &path) {
                bail!("journal と state の管理パスが衝突しています: {}", journal.display());
            }
            protected.push(path);
        }
        protected.push(state);
    }
    let control_paths = protected.len();
    for def in definitions {
        let paths = normalized(&def.paths)?;
        if protected[..control_paths].iter().any(|control| overlap(control, &paths)) {
            bail!("paths と保護パスが衝突しています: {}", paths.display());
        }
        protected.push(paths);
        if let Some(source) = &def.source {
            protected.push(managed(source)?);
        }
    }
    let outputs: Vec<_> = definitions.iter().map(|def| managed(&def.out)).collect::<Result<_>>()?;
    let mut destinations = Vec::new();
    for (index, def) in definitions.iter().enumerate() {
        let paths = artifacts(&def.out)?;
        for path in &paths {
            for other in protected
                .iter()
                .chain(outputs.iter().enumerate().filter_map(|(i, p)| (i != index).then_some(p)))
                .chain(&destinations)
            {
                if overlap(path, other) {
                    bail!(
                        "木の出力先と保護パスが衝突しています: {} / {}",
                        path.display(),
                        other.display()
                    );
                }
            }
        }
        destinations.extend(paths);
    }
    Ok(())
}

/// 木の出力 dir と、その中で書き換える成果物。どれかがリンクなら拒否する。
pub(super) fn artifacts(out: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = vec![managed(out)?];
    for name in [
        "STATUS",
        "tree.lock",
        "own_nodes.tsv",
        "node_moves.tsv",
        "registration_status.json",
        "tree.db",
        "tree_rule.db",
        "tree_bp_min.db",
        "report.txt",
        "paths.record.tsv",
    ] {
        paths.push(managed(&out.join(name))?);
    }
    if out.try_exists()? {
        for entry in std::fs::read_dir(out)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('d') && name.ends_with(".jsonl") {
                paths.push(managed(&entry.path())?);
            }
        }
    }
    Ok(paths)
}

/// キューの対象の出力の根が台帳や state の中に無いことを確かめ、正規化した根と保護パスを返す。
/// 根が state の親でもよい (対象ごとの出力先は受付で保護パスと照合する)。
pub(super) fn reject_out_root(
    journal: &Path,
    state: &Path,
    out_root: &Path,
) -> Result<(PathBuf, Vec<PathBuf>)> {
    let out_root = managed(out_root)?;
    let protected = vec![managed(journal)?, managed(state)?];
    for path in &protected {
        if out_root.starts_with(path) {
            bail!(
                "出力の根と保護パスが衝突しています: {} / {}",
                out_root.display(),
                path.display()
            );
        }
    }
    Ok((out_root, protected))
}

/// 対象の出力先が保護パスと重ならないことを確かめる。
pub(super) fn reject_protected(out: &Path, protected: &[PathBuf]) -> Result<()> {
    let out = managed(out)?;
    for path in protected {
        if overlap(&out, path) {
            bail!("対象の出力先と保護パスが衝突しています: {} / {}", out.display(), path.display());
        }
    }
    Ok(())
}

/// serve の出力の根の lock が通常のファイルとしてあるか。リンクなどは serve と replay で
/// 別のファイルを lock し得るので拒む。
pub(super) fn serve_lock_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => bail!("serve の lock が通常のファイルではありません: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// 出力先の上にある serve の出力の根の lock を取る。serve が動いていれば失敗し、
/// replay の間は serve がその根で起動できない。
pub(super) fn lock_serve_roots(out: &Path) -> Result<Vec<super::journal::LockedFile>> {
    let out = normalized(out)?;
    let mut locks = Vec::new();
    for dir in out.ancestors() {
        let path = dir.join(super::targets::OUT_ROOT_LOCK);
        if serve_lock_exists(&path)? {
            locks.push(
                super::journal::LockedFile::open(&path).with_context(|| {
                    format!("serve が出力の根を使っています: {}", dir.display())
                })?,
            );
        }
    }
    Ok(locks)
}
