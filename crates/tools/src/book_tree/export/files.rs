use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub(super) const SUFFIXES: [&str; 4] = [
    ".db",
    ".default.explore.txt",
    ".random.explore.txt",
    ".annot.json",
];
pub(super) fn outputs(prefix: &Path) -> Vec<PathBuf> {
    SUFFIXES
        .iter()
        .map(|s| {
            let mut p = prefix.as_os_str().to_owned();
            p.push(s);
            PathBuf::from(p)
        })
        .collect()
}
fn resolved(path: &Path) -> Result<PathBuf> {
    Ok(crate::common::dedup::canonicalize_maybe_new(path)?)
}

pub(super) fn guard(outputs: &[PathBuf], inputs: &[PathBuf], force: bool) -> Result<()> {
    for output in outputs {
        ensure!(!output.is_dir(), "output is a directory: {}", output.display());
        let exists = match output.symlink_metadata() {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        ensure!(force || !exists, "output exists: {} (use --force)", output.display());
        let target = resolved(output)?;
        for input in inputs {
            ensure!(
                target != resolved(input)?
                    && !(output.exists()
                        && input.exists()
                        && same_file::is_same_file(output, input)?),
                "output is an input: {}",
                output.display()
            );
        }
    }
    Ok(())
}
pub(super) fn write(
    outputs: &[PathBuf],
    inputs: &[PathBuf],
    force: bool,
    payloads: &[Vec<u8>],
) -> Result<()> {
    write_with(outputs, inputs, force, payloads, || Ok(()), |_| Ok(()))
}
fn write_with(
    outputs: &[PathBuf],
    inputs: &[PathBuf],
    force: bool,
    payloads: &[Vec<u8>],
    before_commit: impl FnOnce() -> Result<()>,
    mut before_persist: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    ensure!(outputs.len() == payloads.len(), "output count mismatch");
    guard(outputs, inputs, force)?;
    let mut created_dirs = Vec::new();
    let mut backups = Vec::new();
    let mut committed = Vec::new();
    let result = (|| -> Result<()> {
        let mut temporary = Vec::new();
        for (path, bytes) in outputs.iter().zip(payloads) {
            let parent =
                path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            create_parents(parent, &mut created_dirs)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(bytes)?;
            file.flush()?;
            temporary.push(file);
        }
        before_commit()?;
        guard(outputs, inputs, force)?;
        for path in outputs {
            match path.symlink_metadata() {
                Ok(_) => {
                    ensure!(force, "output exists: {} (use --force)", path.display());
                    let parent = path
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new("."));
                    let (file, backup) =
                        tempfile::NamedTempFile::new_in(parent)?.keep().map_err(|e| e.error)?;
                    drop(file);
                    if let Err(error) = std::fs::rename(path, &backup) {
                        let cleanup = std::fs::remove_file(&backup);
                        return Err(error).with_context(|| {
                            format!(
                                "退避できません: {} -> {}; 一時ファイル削除: {cleanup:?}",
                                path.display(),
                                backup.display()
                            )
                        });
                    }
                    backups.push((path.clone(), backup));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for (i, (file, path)) in temporary.into_iter().zip(outputs).enumerate() {
            before_persist(i)?;
            file.persist_noclobber(path)
                .map_err(|e| e.error)
                .with_context(|| format!("確定できません: {}", path.display()))?;
            committed.push(path);
        }
        Ok(())
    })();
    if let Err(mut error) = result {
        for path in committed.into_iter().rev() {
            if let Err(e) = std::fs::remove_file(path) {
                error =
                    error.context(format!("新しい出力を削除できません: {}: {e}", path.display()));
            }
        }
        for (path, backup) in backups.into_iter().rev() {
            if let Err(e) = std::fs::rename(&backup, &path) {
                error = error.context(format!(
                    "復元できません: {} -> {}: {e}; 退避先: {}",
                    backup.display(),
                    path.display(),
                    backup.display()
                ));
            }
        }
        for dir in created_dirs.into_iter().rev() {
            if let Err(e) = std::fs::remove_dir(&dir)
                && e.kind() != std::io::ErrorKind::DirectoryNotEmpty
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error = error
                    .context(format!("親ディレクトリを削除できません: {}: {e}", dir.display()));
            }
        }
        return Err(error);
    }
    let mut errors = Vec::new();
    for (_, backup) in backups {
        if let Err(e) = std::fs::remove_file(&backup) {
            errors.push(format!("退避先を削除できません: {}: {e}", backup.display()));
        }
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}

fn create_parents(path: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_parents(parent, created)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => created.push(path.to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_and_staging_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        std::fs::write(&input, "original").unwrap();
        #[cfg(unix)]
        {
            let link = d.path().join("symlink");
            std::os::unix::fs::symlink(&input, &link).unwrap();
            assert!(guard(&[link], std::slice::from_ref(&input), true).is_err());
        }
        let out = outputs(&d.path().join("book"));
        let data = vec![b"new".to_vec(); 4];
        assert!(guard(std::slice::from_ref(&input), std::slice::from_ref(&input), true).is_err());
        let link = d.path().join("hardlink");
        std::fs::hard_link(&input, &link).unwrap();
        assert!(guard(&[link], std::slice::from_ref(&input), true).is_err());
        std::fs::create_dir(&out[2]).unwrap();
        assert!(write(&out, &[], true, &data).is_err());
        std::fs::remove_dir(&out[2]).unwrap();
        let n = std::fs::read_dir(d.path()).unwrap().count();
        assert!(
            write_with(&out, &[], false, &data, || anyhow::bail!("injected failure"), |_| Ok(()))
                .is_err()
        );
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), n);
        write(&out, &[], false, &data).unwrap();
        assert!(write(&out, &[], false, &data).is_err());
        write(&out, &[], true, &data).unwrap();
        let nested = outputs(&d.path().join("new/nested/book"));
        write(&nested, &[], false, &data).unwrap();
        assert!(nested.iter().all(|p| p.is_file()));
        assert_eq!(std::fs::read_to_string(input).unwrap(), "original");
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;

    #[test]
    fn failed_second_and_fourth_commit_restore_outputs() {
        for force in [false, true] {
            for fail_at in [1, 3] {
                let dir = tempfile::tempdir().unwrap();
                let prefix = dir.path().join("new/nested/book");
                let paths = outputs(&prefix);
                if force {
                    std::fs::create_dir_all(prefix.parent().unwrap()).unwrap();
                    for (i, path) in paths.iter().enumerate() {
                        std::fs::write(path, format!("old {i}")).unwrap();
                    }
                }
                let error = write_with(
                    &paths,
                    &[],
                    force,
                    &vec![b"new".to_vec(); 4],
                    || Ok(()),
                    |i| {
                        ensure!(i != fail_at, "injected persist failure");
                        Ok(())
                    },
                )
                .unwrap_err();
                assert!(error.to_string().contains("injected persist failure"));
                if force {
                    for (i, path) in paths.iter().enumerate() {
                        assert_eq!(std::fs::read_to_string(path).unwrap(), format!("old {i}"));
                    }
                    assert_eq!(std::fs::read_dir(prefix.parent().unwrap()).unwrap().count(), 4);
                } else {
                    assert!(!dir.path().join("new").exists());
                }
            }
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn persist_error_drops_staging_and_preserves_nonempty_parent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = outputs(&dir.path().join("new/book"));
        let error = write_with(
            &paths,
            &[],
            false,
            &vec![b"new".to_vec(); 4],
            || Ok(()),
            |i| {
                if i == 1 {
                    std::fs::create_dir(&paths[i])?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("確定できません"));
        let remaining: Vec<_> = std::fs::read_dir(dir.path().join("new"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(remaining, [paths[1].clone()]);
    }

    #[test]
    fn failed_restore_keeps_backup_and_reports_path() {
        let dir = tempfile::tempdir().unwrap();
        let paths = outputs(&dir.path().join("book"));
        for path in &paths {
            std::fs::write(path, "old").unwrap();
        }
        let error = write_with(
            &paths,
            &[],
            true,
            &vec![b"new".to_vec(); 4],
            || Ok(()),
            |i| {
                if i == 1 {
                    std::fs::create_dir(&paths[i])?;
                    anyhow::bail!("injected failure");
                }
                Ok(())
            },
        )
        .unwrap_err();
        let backup = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| !paths.contains(p))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "old");
        assert!(error.to_string().contains(&backup.display().to_string()));
        for i in [0, 2, 3] {
            assert_eq!(std::fs::read_to_string(&paths[i]).unwrap(), "old");
        }
    }
}
