//! Floodgate棋譜サーバーからのダウンロードユーティリティ

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, HeaderValue};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

/// Floodgate root。サイトは http アクセスを 301 で https へ誘導するため、
/// リダイレクトを避けて https を直接指す。
pub const DEFAULT_ROOT: &str = "https://wdoor.c.u-tokyo.ac.jp/shogi/x/";

/// レーティングページの相対パス接頭辞。実体は日次生成の日付スタンプ付き
/// `rating/players-floodgate-YYYYMMDD.html`。
const RATING_PAGE_REL_PREFIX: &str = "rating/players-floodgate-";

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
            // 絶対パスは末尾の "/x/" 以降を相対パスとして返す。floodgate サーバの
            // www root は変わりうる (例: /home/shogi-server/www/x/ →
            // /home/shogi/work/x/) ため prefix をハードコードせず "/x/" を anchor に
            // して吸収する。"/x/" が無ければ既に YYYY/MM/DD/... 相対形式とみなす。
            if let Some(pos) = path.rfind("/x/") {
                return Some(path[pos + "/x/".len()..].to_string());
            }
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

/// 指定日 (YYYYMMDD) の Floodgate レーティングページ URL を root から構築する。
pub fn rating_page_url(root: &str, date_yyyymmdd: &str) -> Result<String> {
    join_url(root, &format!("{RATING_PAGE_REL_PREFIX}{date_yyyymmdd}.html"))
}

/// 直近のレーティングページを取得し (URL, HTML) を返す。
///
/// ページは日次生成の日付スタンプ付きで当日分が未生成のこともあるため、
/// 今日から数日遡って最初に取得できたページを採用する。
pub fn fetch_latest_rating_page(client: &Client) -> Result<(String, String)> {
    const LOOKBACK_DAYS: i64 = 7;
    let today = chrono::Local::now().date_naive();
    let mut last_err: Option<anyhow::Error> = None;
    for back in 0..=LOOKBACK_DAYS {
        let date = today - chrono::Duration::days(back);
        let url = rating_page_url(DEFAULT_ROOT, &date.format("%Y%m%d").to_string())?;
        match http_get_text(client, &url) {
            Ok(html) => return Ok((url, html)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("no candidate dates"))
        .context(format!(
            "直近 {LOOKBACK_DAYS} 日分の Floodgate レーティングページを取得できませんでした"
        )))
}

/// Floodgate レーティングページ HTML から (プレイヤー名, レーティング) のリストを抽出。
///
/// ページはプレイヤー 1 人につき以下のテーブル行を持つ:
/// ```html
/// <td class="name"><a href="...">PlayerName</a></td>
/// <td class="rate">4550</td>
/// ```
/// 表示名 (`<td class="name">` 内アンカーのテキスト) と直後に現れる
/// `<td class="rate">` の数値をペアにする。
pub fn parse_rating_page(html: &str) -> Vec<(String, f64)> {
    let mut results = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in html.lines() {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("<td class=\"name\">") {
            pending_name = anchor_text(rest);
        } else if let Some(rest) = s.strip_prefix("<td class=\"rate\">") {
            // セル先頭から最初の '<' (</td>) 直前までを数値とみなす。負レートも取りこぼさない。
            let val = rest.split('<').next().unwrap_or("").trim();
            if let Ok(rating) = val.parse::<f64>()
                && let Some(name) = pending_name.take()
            {
                results.push((name, rating));
            }
        }
    }
    results
}

/// `<a href="...">TEXT</a>...` から TEXT を抽出する。
fn anchor_text(s: &str) -> Option<String> {
    let open_end = s.find('>')? + 1;
    let close = s[open_end..].find("</a>")? + open_end;
    let text = s[open_end..close].trim();
    (!text.is_empty()).then(|| text.to_string())
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

/// プレイヤーファイルを読み込みパターンリストとして返す。
/// 形式: 1行1名、または TSV (`name\trating`) の場合は最初のフィールドを名前として使用。
/// 部分一致フィルタ用（[`player_matches`] と組み合わせて使う）。
pub fn load_player_patterns(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read player file: {}", path.display()))?;
    let patterns: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').next().unwrap_or(l).to_string())
        .collect();
    Ok(patterns)
}

/// プレイヤー名がパターンリストのいずれかに部分一致するか判定。
pub fn player_matches(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| name.contains(p.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_index_lines() {
        let sample = b"2010/01/01\t...\t/home/shogi-server/www/x/2010/01/01/wdoor+floodgate-900-0+Bonanza+gps_l+20100101000000.csa\t115\n2026/04/13\t...\t/home/shogi/work/x/2026/04/13/wdoor+floodgate-300-10F+910+foo_human+20260413203001.csa\t70\n# comment\nfoo.txt\n2025/01/floodgate-3600-20250101-0001.csa\n";
        let v = parse_index_lines(&sample[..]).unwrap();
        assert_eq!(v.len(), 3);
        assert!(v[0].starts_with("2010/01/01/") && v[0].ends_with(".csa"));
        // www root が変わった新形式 (/home/shogi/work/x/) も "/x/" anchor で吸収
        assert!(v[1].starts_with("2026/04/13/") && v[1].ends_with(".csa"));
        assert!(v[2].contains("2025/01/"));
    }
    #[test]
    fn test_join_url() {
        let u = join_url(DEFAULT_ROOT, "2025/01/a.csa").unwrap();
        assert!(u.starts_with("https://"));
        assert!(u.contains("/shogi/x/2025/01/a.csa"));
    }
    #[test]
    fn test_rating_page_url() {
        let u = rating_page_url(DEFAULT_ROOT, "20260620").unwrap();
        assert_eq!(
            u,
            "https://wdoor.c.u-tokyo.ac.jp/shogi/x/rating/players-floodgate-20260620.html"
        );
    }
    #[test]
    fn test_parse_rating_page() {
        let html = r#"
        <tr>
          <td class="name"><a href="/shogi/x/2026/player/Frieren+abc1234.html">Frieren</a></td>
          <td class="rate">4550</td>
          <td class="last_modified">2026-06-19 03:00:00</td>
        </tr>
        <tr>
          <td class="name"><a href="/shogi/x/2026/player/Reze+def5678.html">Reze</a></td>
          <td class="rate">4474</td>
          <td class="last_modified">2026-06-18 03:00:00</td>
        </tr>
        <tr>
          <td class="name"><a href="/shogi/x/2026/player/BCT+aaa9999.html">BCT</a></td>
          <td class="rate">-1769</td>
          <td class="last_modified">2026-06-17 03:00:00</td>
        </tr>
        "#;
        let result = parse_rating_page(html);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "Frieren");
        assert!((result[0].1 - 4550.0).abs() < 0.01);
        assert_eq!(result[1].0, "Reze");
        assert!((result[1].1 - 4474.0).abs() < 0.01);
        // 負レートも取りこぼさない
        assert_eq!(result[2].0, "BCT");
        assert!((result[2].1 + 1769.0).abs() < 0.01);
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
