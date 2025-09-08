use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoloadMode {
    Strict,
    Permissive,
    Off,
}

#[derive(Debug)]
pub struct ManifestResolved {
    pub path: PathBuf,
    pub scope: &'static str, // "file" | "part" | "dir"
    pub verified: bool,
    pub reason: String,
    pub json: Value,
    pub output_sha256: Option<String>,
    pub output_bytes: Option<u64>,
}

/// Compute SHA-256 of a file.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn read_json(path: &Path) -> Option<Value> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn drop_compression_ext(file: &str) -> &str {
    if let Some(stripped) = file.strip_suffix(".gz") {
        return stripped;
    }
    if let Some(stripped) = file.strip_suffix(".zst") {
        return stripped;
    }
    file
}

fn drop_jsonl_ext(file: &str) -> Option<&str> {
    file.strip_suffix(".jsonl")
}

fn is_part_stem(stem: &str) -> Option<(&str, &str)> {
    // return (base, idx) if matches *.part-XXXX
    if let Some(pos) = stem.rfind(".part-") {
        let (base, rest) = stem.split_at(pos);
        let idx = &rest[6..]; // skip ".part-"
        if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) {
            return Some((base, idx));
        }
    }
    None
}

fn candidates_for_input(input_path: &Path) -> Vec<(PathBuf, &'static str)> {
    let dir = input_path.parent().unwrap_or_else(|| Path::new("."));
    let file = input_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let without_comp = drop_compression_ext(file);
    let mut cands: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(stem) = drop_jsonl_ext(without_comp) {
        if let Some((base, _idx)) = is_part_stem(stem) {
            // exact part manifest, then parent stem manifest
            cands.push((dir.join(format!("{}.manifest.json", stem)), "part"));
            cands.push((dir.join(format!("{}.manifest.json", base)), "file"));
        } else {
            // non-part
            cands.push((dir.join(format!("{}.manifest.json", stem)), "file"));
        }
    }
    // dir generic manifest.json (last resort)
    cands.push((dir.join("manifest.json"), "dir"));
    // dedup existing paths, keep order
    let mut seen = std::collections::HashSet::new();
    cands
        .into_iter()
        .filter(|(p, _)| p.exists())
        .filter(|(p, _)| seen.insert(p.clone()))
        .collect()
}

fn verify_against_input(manifest: &Value, input: &Path) -> Result<VerifyResult, std::io::Error> {
    let input_meta = std::fs::metadata(input)?;
    let input_size = input_meta.len();
    let m_bytes = manifest.get("output_bytes").and_then(|v| v.as_u64());
    let m_sha = manifest.get("output_sha256").and_then(|v| v.as_str()).map(|s| s.to_string());

    match (m_bytes, m_sha) {
        (Some(bytes), Some(sha)) => {
            if bytes != input_size {
                return Ok(VerifyResult::Mismatch);
            }
            let calc = sha256_file(input)?;
            if calc == sha {
                Ok(VerifyResult::Ok { bytes, sha })
            } else {
                Ok(VerifyResult::Mismatch)
            }
        }
        _ => Ok(VerifyResult::Undecidable),
    }
}

pub enum VerifyOutcome {
    Ok,
    Undecidable,
    Mismatch,
}

enum VerifyResult {
    Ok { bytes: u64, sha: String },
    Undecidable,
    Mismatch,
}

/// Resolve a manifest for the given input path according to the B-plan rules.
/// Returns Ok(Some(...)) when a (verified or permissive) match is found, Ok(None) when mode=Off
/// or no acceptable candidate exists.
pub fn resolve_manifest(
    input_path: &Path,
    mode: AutoloadMode,
) -> Result<Option<ManifestResolved>, Box<dyn std::error::Error>> {
    if mode == AutoloadMode::Off {
        return Ok(None);
    }
    let cands = candidates_for_input(input_path);
    for (cand, scope) in cands {
        if let Some(json) = read_json(&cand) {
            match verify_against_input(&json, input_path) {
                Ok(VerifyResult::Ok { bytes, sha }) => {
                    return Ok(Some(ManifestResolved {
                        path: cand,
                        scope,
                        verified: true,
                        reason: "sha/size verified".into(),
                        output_bytes: Some(bytes),
                        output_sha256: Some(sha),
                        json,
                    }));
                }
                Ok(VerifyResult::Undecidable) => {
                    if mode == AutoloadMode::Permissive {
                        return Ok(Some(ManifestResolved {
                            path: cand,
                            scope,
                            verified: false,
                            reason: "no sha/bytes; permissive accept".into(),
                            output_bytes: None,
                            output_sha256: None,
                            json,
                        }));
                    }
                }
                Ok(VerifyResult::Mismatch) => {
                    // try next candidate
                }
                Err(_) => {
                    // unreadable candidate; skip
                }
            }
        }
    }
    Ok(None)
}

/// Verify an already loaded manifest JSON against a specific input file.
pub fn verify_manifest_json_against_input(
    manifest: &Value,
    input_path: &Path,
) -> Result<VerifyOutcome, std::io::Error> {
    match verify_against_input(manifest, input_path)? {
        VerifyResult::Ok { .. } => Ok(VerifyOutcome::Ok),
        VerifyResult::Undecidable => Ok(VerifyOutcome::Undecidable),
        VerifyResult::Mismatch => Ok(VerifyOutcome::Mismatch),
    }
}
