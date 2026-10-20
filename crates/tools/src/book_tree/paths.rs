use super::model::Definition;
use crate::common::dedup::canonicalize_maybe_new;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

fn normalized(path: &Path) -> Result<PathBuf> {
    let path = canonicalize_maybe_new(path)?;
    #[cfg(windows)]
    let path = PathBuf::from(path.as_os_str().to_string_lossy().to_lowercase());
    Ok(path)
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub(super) fn reject_collisions(
    journal: &Path,
    state: Option<&Path>,
    definitions: &[&Definition],
) -> Result<()> {
    let journal = normalized(journal)?;
    let mut protected = vec![journal.clone()];
    if let Some(state) = state {
        let state = normalized(state)?;
        for name in [
            "inbox",
            "restart",
            "lock",
            "workers",
            "stop",
            "STATUS",
            "ALERT",
            "progress.json",
            "accepted.json",
        ] {
            let path = normalized(&state.join(name))?;
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
    }
    let outputs: Vec<_> =
        definitions.iter().map(|def| normalized(&def.out)).collect::<Result<_>>()?;
    let mut destinations = Vec::new();
    for (index, def) in definitions.iter().enumerate() {
        let mut paths = vec![outputs[index].clone()];
        for name in [
            "STATUS",
            "tree.lock",
            "own_nodes.tsv",
            "registration_status.json",
            "tree.db",
            "tree_rule.db",
            "tree_bp_min.db",
            "report.txt",
        ] {
            paths.push(normalized(&def.out.join(name))?);
        }
        if def.out.try_exists()? {
            for entry in std::fs::read_dir(&def.out)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('d') && name.ends_with(".jsonl") {
                    paths.push(normalized(&entry.path())?);
                }
            }
        }
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
