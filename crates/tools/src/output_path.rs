//! 入力を truncate しないための出力パス検査。

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn canonicalize_predicted_path(path: &Path) -> Result<PathBuf> {
    crate::common::dedup::canonicalize_maybe_new(path)
        .with_context(|| format!("Failed to canonicalize {}", path.display()))
}

fn is_same_file(a: &Path, b: &Path) -> Result<bool> {
    match same_file::is_same_file(a, b) {
        Ok(same) => Ok(same),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!("Failed to compare file identities: {} and {}", a.display(), b.display())
        }),
    }
}

fn ensure_output_not_symlink(output: &Path) -> Result<()> {
    match fs::symlink_metadata(output) {
        Ok(meta) if meta.file_type().is_symlink() => {
            anyhow::bail!("Output path is a symlink: {}", output.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to inspect output {}", output.display()))
        }
    }
}

/// 出力が入力実体を指す場合は、truncate 前に拒否する。
pub fn ensure_safe_output_path(output: &Path, input: &Path) -> Result<()> {
    let canonical_input = input
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize input {}", input.display()))?;
    ensure_output_not_symlink(output)?;
    let predicted = canonicalize_predicted_path(output)?;
    if predicted == canonical_input || is_same_file(output, &canonical_input)? {
        anyhow::bail!(
            "Output path resolves to input file: {} -> {}",
            output.display(),
            canonical_input.display()
        );
    }
    Ok(())
}

/// 全出力と読み取り専用資源の衝突、および出力同士の衝突を作成前に検査する。
pub fn ensure_safe_output_paths(outputs: &[&Path], inputs: &[&Path]) -> Result<()> {
    for (index, output) in outputs.iter().enumerate() {
        ensure_output_not_symlink(output)?;
        for input in inputs {
            ensure_safe_output_path(output, input)?;
        }
        for other in &outputs[..index] {
            ensure_distinct_output_paths(output, other)?;
        }
    }
    Ok(())
}

/// 全出力を切り詰めずに開き、資源・出力間の handle 同一性を確認してから空にする。
/// 同時アクセスの排他や失敗時の復元は行わず、エラー時に新規の空ファイルが残る場合がある。
pub fn create_safe_output_files(outputs: &[&Path], inputs: &[&Path]) -> Result<Vec<File>> {
    ensure_safe_output_paths(outputs, inputs)?;
    let input_handles: Vec<same_file::Handle> = inputs
        .iter()
        .map(|path| {
            same_file::Handle::from_path(path)
                .with_context(|| format!("Failed to open input identity {}", path.display()))
        })
        .collect::<Result<_>>()?;
    let mut files = Vec::with_capacity(outputs.len());
    let mut output_handles = Vec::with_capacity(outputs.len());
    for (index, path) in outputs.iter().enumerate() {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("Failed to open output {}", path.display()))?;
        let handle = same_file::Handle::from_file(file.try_clone()?)
            .with_context(|| format!("Failed to read output identity {}", path.display()))?;
        for (input, input_handle) in inputs.iter().zip(&input_handles) {
            anyhow::ensure!(
                handle != *input_handle,
                "Output path resolves to input file: {} -> {}",
                path.display(),
                input.display()
            );
        }
        for (other, other_handle) in outputs[..index].iter().zip(&output_handles) {
            anyhow::ensure!(
                handle != *other_handle,
                "Output paths resolve to the same file: {} and {}",
                path.display(),
                other.display()
            );
        }
        ensure_output_not_symlink(path)?;
        files.push(file);
        output_handles.push(handle);
    }
    // Windows の file identity は比較中も handle を保持する必要がある。
    for (path, file) in outputs.iter().zip(&files) {
        file.set_len(0)
            .with_context(|| format!("Failed to truncate output {}", path.display()))?;
    }
    Ok(files)
}

/// 単一出力を資源の alias でないと確認してから作成または切り詰める。
pub fn create_safe_output_file(output: &Path, inputs: &[&Path]) -> Result<File> {
    create_safe_output_files(&[output], inputs)?
        .pop()
        .context("Output file was not opened")
}

/// 二つの出力パスが同じ実体または同じ生成予定パスなら拒否する。
pub fn ensure_distinct_output_paths(a: &Path, b: &Path) -> Result<()> {
    let predicted_a = canonicalize_predicted_path(a)?;
    let predicted_b = canonicalize_predicted_path(b)?;
    if predicted_a == predicted_b || is_same_file(a, b)? {
        anyhow::bail!("Output paths resolve to the same file: {} and {}", a.display(), b.display());
    }
    Ok(())
}

/// 作成済みファイル群の実体をペア毎に比較し、同一実体があれば拒否する。
/// 未作成パスの予測比較は case-insensitive filesystem の alias
/// (`Score.bin` と `score.bin` 等) を検出できないため、staging ファイルを
/// 作成した直後・書き込み前にこの実体検査を通すこと。
pub fn ensure_created_paths_distinct(paths: &[&Path]) -> Result<()> {
    for i in 0..paths.len() {
        for j in i + 1..paths.len() {
            ensure_not_same_entity(paths[i], paths[j])?;
        }
    }
    Ok(())
}

/// 作成済みパス群と対象パス群の実体をクロス比較し、同一実体があれば拒否する。
/// staging の作成が別出力の最終パスを実体化させる alias
/// (例: `out_scores=X` の staging `X.partial` が最終出力 `x.partial` と一致)
/// は staging 同士の比較では検出できないため、staging×最終出力にも掛けること。
/// 対象パスが未作成 (NotFound) のペアは不一致として扱う。
pub fn ensure_no_entity_overlap(created: &[&Path], targets: &[&Path]) -> Result<()> {
    for created_path in created {
        for target in targets {
            ensure_not_same_entity(created_path, target)?;
        }
    }
    Ok(())
}

fn ensure_not_same_entity(a: &Path, b: &Path) -> Result<()> {
    let same = match same_file::is_same_file(a, b) {
        Ok(same) => same,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to compare file identities: {} and {}", a.display(), b.display())
            });
        }
    };
    anyhow::ensure!(
        !same,
        "Output paths resolve to the same file: {} and {}",
        a.display(),
        b.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn safe_create_preserves_input_and_replaces_distinct_output() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.bin");
        let output = dir.path().join("output.jsonl");
        fs::write(&input, b"resource")?;
        fs::write(&output, b"old longer output")?;
        let mut file = create_safe_output_file(&output, &[&input])?;
        file.write_all(b"new")?;
        file.flush()?;
        assert_eq!(fs::read(input)?, b"resource");
        assert_eq!(fs::read(output)?, b"new");
        Ok(())
    }

    #[test]
    fn later_resource_hardlink_leaves_all_existing_outputs_unchanged() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("model.bin");
        let first = dir.path().join("first.jsonl");
        let second = dir.path().join("second.jsonl");
        fs::write(&input, b"resource")?;
        fs::write(&first, b"previous output")?;
        fs::hard_link(&input, &second)?;
        assert!(create_safe_output_files(&[&first, &second], &[&input]).is_err());
        assert_eq!(fs::read(input)?, b"resource");
        assert_eq!(fs::read(first)?, b"previous output");
        assert_eq!(fs::read(second)?, b"resource");
        Ok(())
    }

    #[test]
    fn later_output_hardlink_leaves_all_existing_outputs_unchanged() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.bin");
        let first = dir.path().join("first.jsonl");
        let second = dir.path().join("second.jsonl");
        fs::write(&input, b"resource")?;
        fs::write(&first, b"previous output")?;
        fs::hard_link(&first, &second)?;
        assert!(create_safe_output_files(&[&first, &second], &[&input]).is_err());
        assert_eq!(fs::read(input)?, b"resource");
        assert_eq!(fs::read(first)?, b"previous output");
        assert_eq!(fs::read(second)?, b"previous output");
        Ok(())
    }

    #[test]
    fn later_open_error_does_not_truncate_first_output() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.bin");
        let first = dir.path().join("first.jsonl");
        let missing_parent = dir.path().join("missing/second.jsonl");
        fs::write(&input, b"resource")?;
        fs::write(&first, b"previous output")?;
        assert!(create_safe_output_files(&[&first, &missing_parent], &[&input]).is_err());
        assert_eq!(fs::read(first)?, b"previous output");
        assert_eq!(fs::read(input)?, b"resource");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn newly_created_case_alias_is_rejected_before_any_truncate() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.bin");
        let first = dir.path().join("first.jsonl");
        let second = dir.path().join("NewOutput.jsonl");
        let third = dir.path().join("newoutput.jsonl");
        fs::write(&input, b"resource")?;
        fs::write(&first, b"previous output")?;
        let error = create_safe_output_files(&[&first, &second, &third], &[&input]).unwrap_err();
        assert!(error.to_string().contains("same file"), "{error:#}");
        assert_eq!(fs::read(first)?, b"previous output");
        assert_eq!(fs::read(input)?, b"resource");
        assert!(second.exists());
        assert!(same_file::is_same_file(second, third)?);
        Ok(())
    }

    #[test]
    fn entity_overlap_detects_hardlinked_paths_and_tolerates_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let created = dir.path().join("created.partial");
        let target = dir.path().join("final.out");
        std::fs::write(&created, [0u8])?;
        std::fs::hard_link(&created, &target)?;
        assert!(ensure_no_entity_overlap(&[&created], &[&target]).is_err());
        assert!(ensure_no_entity_overlap(&[&created], &[&dir.path().join("missing")]).is_ok());
        Ok(())
    }

    #[test]
    fn bare_relative_output_path_is_supported() -> Result<()> {
        let predicted = canonicalize_predicted_path(Path::new("out.psv"))?;
        assert_eq!(predicted.file_name(), Some(std::ffi::OsStr::new("out.psv")));
        assert_eq!(predicted.parent(), Some(std::env::current_dir()?.canonicalize()?.as_path()));
        Ok(())
    }
}
