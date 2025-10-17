use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, ACCEPT_ENCODING};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

/// Floodgate root (HTTP only)
pub const DEFAULT_ROOT: &str = "http://wdoor.c.u-tokyo.ac.jp/shogi/x/";

/// Download text from a URL (HTTP).
pub fn http_get_text(client: &Client, url: &str) -> Result<String> {
    let res = client
        .get(url)
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    anyhow::ensure!(status.is_success(), "HTTP {status} for {url}");
    let text = res.text().with_context(|| format!("read body: {url}"))?;
    Ok(text)
}

/// Download binary to a file if not exists (no clobber). Returns the path.
pub fn http_get_to_file_noclobber(client: &Client, url: &str, out_path: &Path) -> Result<PathBuf> {
    if out_path.exists() {
        return Ok(out_path.to_path_buf());
    }
    if let Some(dir) = out_path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create dir: {}", dir.display()))?;
    }
    let mut res = client
        .get(url)
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    anyhow::ensure!(status.is_success(), "HTTP {status} for {url}");
    let mut f = File::create(out_path).with_context(|| format!("open {}", out_path.display()))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = res.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
    }
    Ok(out_path.to_path_buf())
}

/// Parse 00LIST.floodgate style index text into relative CSA paths (strings like "2025/01/floodgate-... .csa").
pub fn parse_index_lines<R: Read>(r: R) -> Result<Vec<String>> {
    let br = BufReader::new(r);
    let mut v = Vec::new();
    for line in br.lines() {
        let s = line?;
        let s = s.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if let Some(rel) = extract_csa_rel_path(s) {
            v.push(rel);
        }
    }
    Ok(v)
}

fn extract_csa_rel_path(line: &str) -> Option<String> {
    // 00LIST.floodgate は TSV。各行から .csa を含むフィールドを探す。
    for tok in line.split(|c: char| c == '\t' || c.is_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        if let Some(idx) = tok.find(".csa") {
            let path = &tok[..idx + 4];
            // 正規化: /home/shogi-server/www/x/ 以降を相対パスとして返却
            const ABS_PREFIX: &str = "/home/shogi-server/www/x/";
            if let Some(rest) = path.strip_prefix(ABS_PREFIX) {
                return Some(rest.to_string());
            }
            // /shogi/x/ を含む場合はその直後を相対とみなす
            if let Some(pos) = path.find("/shogi/x/") {
                let rel = &path[pos + "/shogi/x/".len()..];
                return Some(rel.to_string());
            }
            // 既に YYYY/MM/DD/... 形式
            if path.starts_with('2') && path.contains('/') {
                return Some(path.to_string());
            }
            // それ以外はパスのまま返す（join_url で失敗する可能性あり）
            return Some(path.to_string());
        }
    }
    None
}

/// Resolve absolute URL from root and a relative path from the index.
pub fn join_url(root: &str, rel: &str) -> Result<String> {
    let base = Url::parse(root)?;
    let url = base.join(rel)?;
    Ok(url.into())
}

/// Suggest a local path to save a CSA given root_dir and index relative path.
pub fn local_path_for(root_dir: &Path, rel: &str) -> PathBuf {
    root_dir.join(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_index_lines() {
        let sample = b"2010/01/01\t...\t/home/shogi-server/www/x/2010/01/01/wdoor+floodgate-900-0+Bonanza+gps_l+20100101000000.csa\t115\n# comment\nfoo.txt\n2025/01/floodgate-3600-20250101-0001.csa\n";
        let v = parse_index_lines(&sample[..]).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].starts_with("2010/01/01/") && v[0].ends_with(".csa"));
        assert!(v[1].contains("2025/01/"));
    }
    #[test]
    fn test_join_url() {
        let u = join_url(DEFAULT_ROOT, "2025/01/a.csa").unwrap();
        assert!(u.starts_with("http://"));
        assert!(u.contains("/shogi/x/2025/01/a.csa"));
    }
}
