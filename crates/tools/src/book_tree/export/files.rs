use anyhow::{Result, ensure};
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
    write_with(outputs, inputs, force, payloads, || Ok(()))
}
fn write_with(
    outputs: &[PathBuf],
    inputs: &[PathBuf],
    force: bool,
    payloads: &[Vec<u8>],
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(outputs.len() == payloads.len(), "output count mismatch");
    guard(outputs, inputs, force)?;
    let mut temporary = Vec::new();
    for (path, bytes) in outputs.iter().zip(payloads) {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.flush()?;
        temporary.push(file);
    }
    before_commit()?;
    guard(outputs, inputs, force)?;
    for (file, path) in temporary.into_iter().zip(outputs) {
        if force {
            file.persist(path)?;
        } else {
            file.persist_noclobber(path)?;
        }
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
        assert!(write_with(&out, &[], false, &data, || anyhow::bail!("injected failure")).is_err());
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
