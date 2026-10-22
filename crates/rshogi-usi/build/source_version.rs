// エンジンのソースの版の材料。build.rs と tests/build_rs_source_version.rs から include! する。

/// エンジンのバイナリに入るソース (workspace root からの相対パス)。版と `-dirty` はこの範囲だけで決める。
const ENGINE_SOURCES: &[&str] =
    &["crates/rshogi-core", "crates/rshogi-book", "crates/rshogi-usi", "Cargo.toml", "Cargo.lock"];

// rshogi-usi 自身の feature のうち結果を変えないもの。rshogi-core へ転送するだけの feature は
// rshogi-core 側 (`rshogi_core::RESULT_FEATURES`) で判定するので、ここでは数えない。
// - mimalloc: アロケータの差し替え
// - allocation-stats: 割り当ての計数 (rshogi-core へも転送)
const USI_RESULT_NEUTRAL_FEATURES: &[&str] = &["default", "mimalloc", "allocation-stats"];

fn git(root: &std::path::Path, args: &[&str], stdin: Option<&str>) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(input) = stdin {
        child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// HEAD のエンジンのソースの tree / blob の id をまとめた id と、commit されていない変更の有無。
/// `root` が git の作業木の最上位でなければ (別の repo の中に展開したソースなど) `None`。
fn engine_source_id(root: &std::path::Path) -> Option<(String, bool)> {
    let top = git(root, &["rev-parse", "--show-toplevel"], None)?;
    if std::fs::canonicalize(top).ok()? != std::fs::canonicalize(root).ok()? {
        return None;
    }
    let mut listing = String::new();
    for path in ENGINE_SOURCES {
        let id = git(root, &["rev-parse", "--verify", "--quiet", &format!("HEAD:{path}")], None)?;
        listing.push_str(&format!("{path} {id}\n"));
    }
    let id = git(root, &["hash-object", "--stdin"], Some(&listing))?;
    let mut status = vec!["status", "--porcelain", "--untracked-files=no", "--"];
    status.extend(ENGINE_SOURCES);
    let dirty = !git(root, &status, None)?.is_empty();
    Some((id, dirty))
}

/// 有効な rshogi-usi の feature のうち、rshogi-core へ転送するだけでなく、結果を変えないと
/// 確かめてもいないものを名前順に返す。
fn usi_result_features(manifest: &str, enabled: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut in_features = false;
    let mut pending: Option<(String, String)> = None;
    let mut entries = Vec::new();
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some((name, mut value)) = pending.take() {
            value.push_str(line);
            if value.contains(']') {
                entries.push((name, value));
            } else {
                pending = Some((name, value));
            }
            continue;
        }
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        if !in_features {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim().to_owned(), value.trim().to_owned());
        if value.contains(']') {
            entries.push((name, value));
        } else {
            pending = Some((name, value));
        }
    }
    let mut features: Vec<String> = entries
        .into_iter()
        .filter(|(name, _)| enabled(name) && !USI_RESULT_NEUTRAL_FEATURES.contains(&name.as_str()))
        .filter(|(_, value)| {
            let deps: Vec<&str> = value
                .trim_matches(|c| c == '[' || c == ']' || c == ' ')
                .split(',')
                .map(|d| d.trim().trim_matches('"'))
                .filter(|d| !d.is_empty())
                .collect();
            deps.is_empty() || !deps.iter().all(|d| d.starts_with("rshogi-core/"))
        })
        .map(|(name, _)| name)
        .collect();
    features.sort();
    features
}
