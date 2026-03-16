//! Floodgate棋譜サーバーからのダウンロードユーティリティ

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, HeaderValue};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

/// Floodgate root (HTTP only)
pub const DEFAULT_ROOT: &str = "http://wdoor.c.u-tokyo.ac.jp/shogi/x/";

/// Floodgate レーティングページ URL
pub const RATING_PAGE_URL: &str =
    "http://wdoor.c.u-tokyo.ac.jp/shogi/LATEST/players-floodgate.html";

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

/// Floodgate レーティングページ HTML から (プレイヤー名, レーティング) のリストを抽出。
///
/// HTML のパターン:
/// ```html
/// <a id="popup1" href="...">PlayerName</a>
/// ...
/// <span id="popup2"> 4550</span>
/// ```
/// 名前 (popup奇数) とレーティング (popup偶数) が交互に出現する。
pub fn parse_rating_page(html: &str) -> Vec<(String, f64)> {
    let mut results = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in html.lines() {
        let s = line.trim();
        // <a id="popupN" href="...">NAME</a>
        if s.starts_with("<a id=\"popup") && s.ends_with("</a>")
            && let Some(gt_pos) = s.rfind("\">") {
                let name = &s[gt_pos + 2..s.len() - 4];
                if !name.is_empty() {
                    pending_name = Some(name.to_string());
                }
            }
        // <span id="popupN"> RATING</span>
        if s.starts_with("<span id=\"popup") && s.ends_with("</span>")
            && let Some(gt_pos) = s.find("\">") {
                let val_str = &s[gt_pos + 2..s.len() - 7];
                if let Ok(rating) = val_str.trim().parse::<f64>()
                    && let Some(name) = pending_name.take() {
                        results.push((name, rating));
                    }
            }
    }
    results
}

/// CSA ファイルの相対パスからプレイヤー名を抽出。
///
/// パス例: `2026/03/17/wdoor+floodgate-300-10F+PlayerA+PlayerB+20260317020006.csa`
/// ファイル名を `+` で分割し、末尾から3番目=先手、2番目=後手。
pub fn players_from_path(rel: &str) -> Option<(&str, &str)> {
    let filename = rel.rsplit('/').next()?;
    let stem = filename.strip_suffix(".csa")?;
    let parts: Vec<&str> = stem.split('+').collect();
    if parts.len() >= 5 {
        Some((parts[parts.len() - 3], parts[parts.len() - 2]))
    } else {
        None
    }
}

/// プレイヤーファイルを読み込み HashSet として返す。
/// 形式: 1行1名、または TSV (`name\trating`) の場合は最初のフィールドを名前として使用。
pub fn load_player_set(path: &Path) -> Result<HashSet<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read player file: {}", path.display()))?;
    let set: HashSet<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').next().unwrap_or(l).to_string())
        .collect();
    Ok(set)
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
    #[test]
    fn test_parse_rating_page() {
        let html = r#"
        <a id="popup1" href="/shogi/view/show-player.cgi?user=Frieren%2Bhash">Frieren</a>
        <script>...</script>
        <span id="popup2"> 4550</span>
        <a id="popup3" href="/shogi/view/show-player.cgi?user=Reze%2Bhash">Reze</a>
        <script>...</script>
        <span id="popup4"> 4474</span>
        "#;
        let result = parse_rating_page(html);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "Frieren");
        assert!((result[0].1 - 4550.0).abs() < 0.01);
        assert_eq!(result[1].0, "Reze");
        assert!((result[1].1 - 4474.0).abs() < 0.01);
    }
    #[test]
    fn test_players_from_path() {
        let p =
            "2026/03/17/wdoor+floodgate-300-10F+Allihn_Condenser+fuwafuwatime+20260317020006.csa";
        let (a, b) = players_from_path(p).unwrap();
        assert_eq!(a, "Allihn_Condenser");
        assert_eq!(b, "fuwafuwatime");

        let p2 = "2010/01/01/wdoor+floodgate-900-0+gps_normal+gps500+20100101000005.csa";
        let (a2, b2) = players_from_path(p2).unwrap();
        assert_eq!(a2, "gps_normal");
        assert_eq!(b2, "gps500");
    }
}
