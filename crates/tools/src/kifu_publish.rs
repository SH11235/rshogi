//! 棋譜を rshogi の棋譜サーバー（`rshogi-csa-server-workers`）へ公開する
//! `kifu_publish` の実装。
//!
//! - wdoor (Floodgate) の対局は、対局 ID だけを渡してサーバーに取得させる
//!   （`POST {api-base}/admin/games/import-wdoor`）。wdoor に棋譜が残っていない局は、
//!   手元の wdoor 版 CSA を本文アップロードする。
//! - `tournament` / `csa_client` の JSONL は wdoor 形式の CSA に変換して本文アップロード
//!   する（`POST {api-base}/admin/games/upload`）。

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, TimeDelta};
use regex::Regex;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use rshogi_csa::{Color, Position};
use rshogi_csa_client::csa_writer::{
    CsaGameEnd, CsaGameHeader, CsaTimeLimit, EndReason, MoveEval, WdoorCsaWriter,
};
use serde::{Deserialize, Serialize};

use crate::common::floodgate::http_client_builder;

/// 管理トークンを読む環境変数名。
pub const ADMIN_TOKEN_ENV: &str = "RSHOGI_ADMIN_TOKEN";
/// API のベース URL（`/api/v1` まで）を読む環境変数名。
pub const API_BASE_ENV: &str = "RSHOGI_API_BASE";

/// 再接続後の断片棋譜に付くファイル名 suffix（`csa_client` の `RecordStatus`）。
const RECONNECT_FRAGMENT_SUFFIX: &str = "_reconnect_fragment";

/// サーバーが受理する wdoor の対局 ID。
static WDOOR_GAME_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^wdoor\+[A-Za-z0-9_.-]+\+[^+/]+\+[^+/]+\+[0-9]{14}$").expect("固定の正規表現")
});

/// wdoor の対局 ID（`wdoor+<ゲーム名>+<先手>+<後手>+<開始時刻 14 桁>`）の形か。
pub fn is_wdoor_game_id(id: &str) -> bool {
    WDOOR_GAME_ID.is_match(id)
}

/// CSA テキストの `$EVENT:` 行の値を返す。
pub fn csa_event(csa: &str) -> Option<&str> {
    csa.lines().find_map(|line| line.trim_end().strip_prefix("$EVENT:"))
}

/// `records_dir` 直下の `*.csa` から wdoor の対局 ID を集め、重複を除いて昇順で返す。
///
/// `_reconnect_fragment` 付きのファイル（再接続後の断片）と、`$EVENT:` が wdoor の
/// ID 形式でないファイル（他サーバーでの対局）は対象外。
pub fn collect_wdoor_ids(records_dir: &Path) -> Result<Vec<String>> {
    let mut ids = BTreeSet::new();
    let entries = std::fs::read_dir(records_dir)
        .with_context(|| format!("ディレクトリを読めません: {}", records_dir.display()))?;
    for entry in entries {
        let path = entry
            .with_context(|| format!("ディレクトリを読めません: {}", records_dir.display()))?
            .path();
        let is_csa = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csa"));
        let is_fragment = path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().ends_with(RECONNECT_FRAGMENT_SUFFIX));
        if !is_csa || is_fragment || !path.is_file() {
            continue;
        }
        if let Some(id) = read_event_line(&path)?
            && is_wdoor_game_id(&id)
        {
            ids.insert(id);
        }
    }
    Ok(ids.into_iter().collect())
}

/// CSA ファイルの `$EVENT:` 行だけを読む（ヘッダーにあるので全文は読まない）。
fn read_event_line(path: &Path) -> Result<Option<String>> {
    let file = File::open(path).with_context(|| format!("開けません: {}", path.display()))?;
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("読めません: {}", path.display()))?;
        if let Some(event) = line.trim_end().strip_prefix("$EVENT:") {
            return Ok(Some(event.to_owned()));
        }
    }
    Ok(None)
}

/// アーカイブ配下から wdoor 版 CSA を探す。
///
/// wdoor と同じ `YYYY/MM/DD/<id>.csa` の階層、次に直下の `<id>.csa` の順に見る。
pub fn find_archive_csa(archive_dir: &Path, wdoor_game_id: &str) -> Option<PathBuf> {
    let file_name = format!("{wdoor_game_id}.csa");
    let dated = wdoor_game_id.len().checked_sub(14).and_then(|start| {
        let stamp = wdoor_game_id.get(start..)?;
        Some(
            archive_dir
                .join(&stamp[0..4])
                .join(&stamp[4..6])
                .join(&stamp[6..8])
                .join(&file_name),
        )
    });
    dated
        .into_iter()
        .chain([archive_dir.join(&file_name)])
        .find(|path| path.is_file())
}

// ---------------------------------------------------------------------------
// JSONL → CSA
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum JsonlLine {
    Meta(MetaLine),
    Move(MoveLine),
    Result(ResultLine),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MetaLine {
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: MetaEngines,
}

#[derive(Deserialize)]
struct MetaSettings {
    #[serde(default)]
    btime: u64,
    #[serde(default)]
    binc: u64,
    #[serde(default)]
    byoyomi: u64,
}

#[derive(Deserialize)]
struct MetaEngines {
    label_black: String,
    label_white: String,
}

#[derive(Deserialize)]
struct MoveLine {
    game_id: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    engine: String,
    elapsed_ms: u64,
    #[serde(default)]
    timed_out: bool,
    eval: Option<EvalLine>,
}

#[derive(Deserialize)]
struct EvalLine {
    score_cp: Option<i32>,
    score_mate: Option<i32>,
    pv: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ResultLine {
    game_id: u32,
    outcome: String,
    reason: String,
    #[serde(default)]
    error: bool,
}

/// JSONL（`tournament` / `csa_client` の `meta` / `move` / `result` 行）を読み、終局した
/// 対局ごとに wdoor 形式の CSA を作って `on_game(game_id, 変換結果)` を呼ぶ。
///
/// `only_game` を指定するとその `game_id` の局だけを変換する。対局は `result` 行が現れた
/// 順に渡し、`result` 行のない局（進行中）は渡さない。1 局の変換失敗は `on_game` へ
/// `Err` として渡して続行し、入力そのものが読めない・JSON として壊れている場合は
/// 全体を `Err` で打ち切る。戻り値は `on_game` を呼んだ局数。
pub fn convert_jsonl<R: BufRead>(
    reader: R,
    event: &str,
    only_game: Option<u32>,
    mut on_game: impl FnMut(u32, Result<String>) -> Result<()>,
) -> Result<usize> {
    let mut meta: Option<MetaLine> = None;
    // 書き込み側は 1 局分の move 行と result 行をまとめて書くので、通常は同時に 1 局分
    // しか保持しない。
    let mut pending: HashMap<u32, Vec<MoveLine>> = HashMap::new();
    let mut games = 0usize;
    for (idx, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("{} 行目を読めません", idx + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: JsonlLine = serde_json::from_str(&line)
            .with_context(|| format!("{} 行目が JSONL として読めません", idx + 1))?;
        match parsed {
            JsonlLine::Meta(m) => meta = Some(m),
            JsonlLine::Move(mv) => {
                if only_game.is_none_or(|id| id == mv.game_id) {
                    pending.entry(mv.game_id).or_default().push(mv);
                }
            }
            JsonlLine::Result(result) => {
                if only_game.is_some_and(|id| id != result.game_id) {
                    continue;
                }
                let moves = pending.remove(&result.game_id).unwrap_or_default();
                let meta = meta.as_ref().context("result 行より前に meta 行がありません")?;
                games += 1;
                on_game(result.game_id, game_to_csa(meta, event, &moves, &result))?;
            }
            JsonlLine::Other => {}
        }
    }
    Ok(games)
}

/// 対局を進めない終局用の擬似指し手（`tournament` が move 行に書く）。
fn is_pseudo_move(move_usi: &str) -> bool {
    matches!(move_usi, "resign" | "win" | "timeout" | "illegal" | "none")
}

/// JSONL の `reason` を CSA の終局理由に直す。
///
/// `tournament` の `win` は入玉宣言勝ちで、宣言した move 行（`move_usi = "win"`）が
/// 残る。`csa_client` の `win` / `lose` はサーバーが理由行を送らなかった決着なので
/// [`EndReason::from_record_result`] に任せる。評価値による裁定勝ち・負け
/// （`adjudication_*`）は投了として記録する。対応する終局理由がないもの（裁定引き分け・
/// エラー・中断など）は公開用の棋譜にできないのでエラー。
fn end_reason_for(reason: &str, declared_win: bool) -> Result<EndReason> {
    let mapped = match reason {
        "win" if declared_win => Some(EndReason::WinDeclaration),
        "timeout" => Some(EndReason::TimeUp),
        "sennichite_perpetual_check" => Some(EndReason::OuteSennichite),
        "adjudication_resign" | "adjudication_win" => Some(EndReason::Resign),
        other => EndReason::from_record_result(other),
    };
    match mapped {
        Some(EndReason::Interrupted) | None => {
            bail!("終局理由 `{reason}` の対局は公開用の CSA に変換できません")
        }
        Some(reason) => Ok(reason),
    }
}

fn game_to_csa(
    meta: &MetaLine,
    event: &str,
    moves: &[MoveLine],
    result: &ResultLine,
) -> Result<String> {
    ensure!(!result.error, "エラーで不成立になった対局です（reason: {}）", result.reason);
    let winner = match result.outcome.as_str() {
        "black_win" => Some(Color::Black),
        "white_win" => Some(Color::White),
        "draw" => None,
        other => bail!("終局していない対局です（outcome: {other}）"),
    };
    let declared_win = moves.last().is_some_and(|m| m.move_usi == "win");
    let reason = end_reason_for(&result.reason, declared_win)?;

    // 時間切れの手は盤面に反映されていないので、擬似指し手と同じく棋譜に入れない。
    let played: Vec<&MoveLine> =
        moves.iter().filter(|m| !m.timed_out && !is_pseudo_move(&m.move_usi)).collect();
    let first = played.first().context("指し手が 1 手もありません")?;
    let initial = Position::from_sfen(&first.sfen_before).context("開始局面を読めません")?;

    let name_of = |side: char| played.iter().find(|m| m.side_to_move == side).map(|m| &m.engine);
    let labels = [&meta.engine_cmd.label_black, &meta.engine_cmd.label_white];
    // 片方が 1 手も指していない局は、meta の 2 つのラベルのうち指した側でない方を使う。
    let other_label = |known: &String| labels.into_iter().find(|label| *label != known);
    let (sente_name, gote_name) = match (name_of('b'), name_of('w')) {
        (Some(b), Some(w)) => (b, w),
        (Some(b), None) => (b, other_label(b).context("後手の対局者名が分かりません")?),
        (None, Some(w)) => (other_label(w).context("先手の対局者名が分かりません")?, w),
        (None, None) => bail!("指し手の手番が読めません"),
    };

    let start_time = DateTime::parse_from_rfc3339(&meta.timestamp)
        .with_context(|| format!("meta の timestamp が不正です: {}", meta.timestamp))?
        .naive_local();
    let settings = &meta.settings;
    let time_limit =
        (settings.btime > 0 || settings.binc > 0 || settings.byoyomi > 0).then_some(CsaTimeLimit {
            total_sec: settings.btime / 1000,
            byoyomi_sec: settings.byoyomi / 1000,
            increment_sec: settings.binc / 1000,
        });
    let header = CsaGameHeader {
        sente_name,
        gote_name,
        event,
        start_time,
        time_limit,
        leading_comment: None,
    };

    let mut writer = WdoorCsaWriter::new(&header, &initial);
    let mut elapsed_total_ms = 0u64;
    for m in &played {
        let side = writer.position().side_to_move;
        let expected = match side {
            Color::Black => 'b',
            Color::White => 'w',
        };
        ensure!(
            m.side_to_move == expected,
            "指し手の手番が局面と合いません（move: {}）",
            m.move_usi
        );
        let empty_pv = Vec::new();
        let eval = m.eval.as_ref().and_then(|eval| {
            Some(MoveEval {
                score_black: black_score(eval, side)?,
                pv_usi: eval.pv.as_ref().unwrap_or(&empty_pv),
            })
        });
        writer
            .push_usi_move(&m.move_usi, m.elapsed_ms / 1000, eval)
            .with_context(|| format!("指し手を CSA に変換できません: {}", m.move_usi))?;
        elapsed_total_ms = elapsed_total_ms.saturating_add(m.elapsed_ms);
    }

    // 終局時刻は記録されていないので、開始時刻 + 消費時間の合計とする。
    let end_time = i64::try_from(elapsed_total_ms)
        .ok()
        .and_then(TimeDelta::try_milliseconds)
        .and_then(|elapsed| start_time.checked_add_signed(elapsed));
    Ok(writer.finish(Some(&CsaGameEnd {
        reason,
        winner,
        end_time,
    })))
}

/// USI の評価値（手番側視点）を先手視点に直す。詰みは `csa_client` と同じ ±100000。
fn black_score(eval: &EvalLine, side: Color) -> Option<i32> {
    let score = match (eval.score_cp, eval.score_mate) {
        (Some(cp), _) => cp,
        (None, Some(mate)) if mate > 0 => 100_000,
        (None, Some(_)) => -100_000,
        (None, None) => return None,
    };
    match side {
        Color::Black => Some(score),
        Color::White => score.checked_neg(),
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// 本文アップロードの `source`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadSource {
    /// 管理者がアップロードした棋譜。
    Upload,
    /// wdoor に残っていない Floodgate 対局を手元アーカイブから入れる。
    Wdoor,
}

/// 取り込み API の応答。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Imported {
    /// サーバーが決めた棋譜 ID。
    pub game_id: String,
    /// 今回新しく保存したか。保存済みで何もしなかったときは `false`。
    pub created: bool,
}

#[derive(Serialize)]
struct ImportWdoorRequest<'a> {
    wdoor_game_id: &'a str,
    force: bool,
}

#[derive(Serialize)]
struct UploadRequest<'a> {
    csa: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<&'a str>,
    source: UploadSource,
}

/// 取り込み API のクライアント。管理トークンは `Authorization` ヘッダーにだけ使い、
/// ログやエラーには出さない。
pub struct ApiClient {
    client: Client,
    api_base: String,
    token: String,
}

impl ApiClient {
    /// `api_base` は `/api/v1` までを含む URL。
    pub fn new(api_base: &str, token: String) -> Result<Self> {
        ensure!(!token.trim().is_empty(), "管理トークンが空です");
        let api_base = api_base.trim_end_matches('/');
        ensure!(
            api_base.starts_with("http://") || api_base.starts_with("https://"),
            "API のベース URL は http:// か https:// で始めてください: {api_base}"
        );
        // 参照取り込みはサーバーが wdoor へ取りに行くぶん待つ。
        let client = http_client_builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("HTTP クライアントを作れません")?;
        Ok(Self {
            client,
            api_base: api_base.to_owned(),
            token,
        })
    }

    /// wdoor の対局 ID を渡してサーバーに取り込ませる。wdoor に棋譜がない（404）ときは
    /// `None`。
    pub fn import_wdoor(&self, wdoor_game_id: &str) -> Result<Option<Imported>> {
        let request = ImportWdoorRequest {
            wdoor_game_id,
            force: false,
        };
        let (status, body) = self.post("admin/games/import-wdoor", &request)?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        parse_imported(status, &body).map(Some)
    }

    /// CSA 本文をアップロードする。
    pub fn upload(&self, csa: &str, event: Option<&str>, source: UploadSource) -> Result<Imported> {
        let request = UploadRequest { csa, event, source };
        let (status, body) = self.post("admin/games/upload", &request)?;
        parse_imported(status, &body)
    }

    fn post(&self, path: &str, request: &impl Serialize) -> Result<(StatusCode, String)> {
        let url = format!("{}/{path}", self.api_base);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(request)?)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let body = response.text().with_context(|| format!("応答を読めません: {url}"))?;
        Ok((status, body))
    }
}

fn parse_imported(status: StatusCode, body: &str) -> Result<Imported> {
    if !status.is_success() {
        let snippet: String = body.chars().take(200).collect();
        bail!("HTTP {status}: {}", snippet.trim());
    }
    serde_json::from_str(body).map_err(|err| anyhow!("応答の JSON が想定と違います: {err}"))
}

/// 送信結果の件数。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// 新しく保存された局数。
    pub created: u32,
    /// サーバーに保存済みで何もしなかった局数（`created = false`）。
    pub skipped: u32,
    /// 失敗した局数。
    pub failed: u32,
}

impl Tally {
    /// 失敗を 1 件数え、標準エラーに記録する。
    pub fn fail(&mut self, label: &str, err: &anyhow::Error) {
        self.failed += 1;
        eprintln!("failed  {label}: {err:#}");
    }

    fn record(&mut self, label: &str, result: Result<Imported>) {
        match result {
            Ok(imported) if imported.created => {
                self.created += 1;
                println!("created {} {label}", imported.game_id);
            }
            Ok(imported) => {
                self.skipped += 1;
                println!("exists  {} {label}", imported.game_id);
            }
            Err(err) => self.fail(label, &err),
        }
    }
}

/// リクエスト間隔を守りながら 1 局ずつ送り、結果を数える。
pub struct Publisher {
    api: ApiClient,
    interval: Duration,
    sent_any: bool,
    /// ここまでの送信結果。
    pub tally: Tally,
}

impl Publisher {
    /// `interval` は連続するリクエストの間に空ける時間。
    pub fn new(api: ApiClient, interval: Duration) -> Self {
        Self {
            api,
            interval,
            sent_any: false,
            tally: Tally::default(),
        }
    }

    fn pace(&mut self) {
        if self.sent_any {
            std::thread::sleep(self.interval);
        }
        self.sent_any = true;
    }

    /// wdoor の 1 局を取り込む。wdoor に棋譜がなければ `archive_dir` の wdoor 版 CSA を
    /// `source: "wdoor"` でアップロードする。
    pub fn publish_wdoor(&mut self, wdoor_game_id: &str, archive_dir: Option<&Path>) {
        self.pace();
        let result = match self.api.import_wdoor(wdoor_game_id) {
            Ok(Some(imported)) => Ok(imported),
            Ok(None) => self.upload_from_archive(wdoor_game_id, archive_dir),
            Err(err) => Err(err),
        };
        self.tally.record(wdoor_game_id, result);
    }

    fn upload_from_archive(
        &mut self,
        wdoor_game_id: &str,
        archive_dir: Option<&Path>,
    ) -> Result<Imported> {
        let archive_dir =
            archive_dir.context("wdoor に棋譜がありません（--archive-dir の指定なし）")?;
        let path = find_archive_csa(archive_dir, wdoor_game_id).with_context(|| {
            format!("wdoor に棋譜がなく、{} にも見つかりません", archive_dir.display())
        })?;
        let csa = std::fs::read_to_string(&path)
            .with_context(|| format!("読めません: {}", path.display()))?;
        ensure!(
            csa_event(&csa) == Some(wdoor_game_id),
            "$EVENT が対局 ID と一致しません: {}",
            path.display()
        );
        self.pace();
        self.api.upload(&csa, None, UploadSource::Wdoor)
    }

    /// CSA 本文を `source: "upload"` でアップロードする。`label` は結果表示用。
    pub fn publish_csa(&mut self, label: &str, csa: &str, event: &str) {
        self.pace();
        let result = self.api.upload(csa, Some(event), UploadSource::Upload);
        self.tally.record(label, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    const ID_A: &str = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";
    const ID_B: &str = "wdoor+floodgate-300-10F+Kristallweizen+RAMU_TF+20260706123000";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn csa_with_event(event: &str) -> String {
        format!("V2.2\nN+A\nN-B\n$EVENT:{event}\n$START_TIME:2026/10/09 00:00:00\nPI\n+\n")
    }

    // -- ID 収集 ------------------------------------------------------------

    #[test]
    fn wdoor_id_pattern_matches_server_contract() {
        assert!(is_wdoor_game_id(ID_A));
        assert!(is_wdoor_game_id(ID_B));
        for bad in [
            "floodgate-300-10F+RAMU+Panda+20261009000001",
            "wdoor+floodgate-300-10F+RAMU+Panda+2026100900000",
            "wdoor+floodgate-300-10F+RAMU+20261009000001",
            "wdoor+floodgate/300+RAMU+Panda+20261009000001",
            "wdoor+floodgate-300-10F+RA/MU+Panda+20261009000001",
            "20261009_000001_RAMU_vs_Panda",
        ] {
            assert!(!is_wdoor_game_id(bad), "{bad}");
        }
    }

    #[test]
    fn collects_sorted_unique_wdoor_ids_from_top_level_csa() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("20261009_000001_RAMU_vs_Panda.csa"), &csa_with_event(ID_A));
        // 同じ対局の重複（再保存など）は 1 件にまとめる。
        write(&root.join("copy.CSA"), &csa_with_event(ID_A));
        write(&root.join("20260706_123000_K_vs_RAMU_TF.csa"), &csa_with_event(ID_B));
        // 対象外: 再接続断片、wdoor 以外の対局、$EVENT なし、拡張子違い、サブディレクトリ。
        write(
            &root.join("20261009_003000_RAMU_vs_X_reconnect_fragment.csa"),
            &csa_with_event("wdoor+floodgate-300-10F+RAMU+X+20261009003000"),
        );
        write(&root.join("local.csa"), &csa_with_event("20261009-local-game"));
        write(&root.join("no_event.csa"), "V2.2\nN+A\nN-B\nPI\n+\n");
        write(
            &root.join("note.txt"),
            &csa_with_event("wdoor+floodgate-300-10F+A+B+20261009010000"),
        );
        write(
            &root.join("jsonl").join("nested.csa"),
            &csa_with_event("wdoor+floodgate-300-10F+C+D+20261009020000"),
        );

        assert_eq!(collect_wdoor_ids(root).unwrap(), [ID_B, ID_A]);
    }

    #[test]
    fn finds_archive_csa_in_dated_layout_then_flat() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(find_archive_csa(root, ID_A), None);

        let flat = root.join(format!("{ID_A}.csa"));
        write(&flat, &csa_with_event(ID_A));
        assert_eq!(find_archive_csa(root, ID_A), Some(flat));

        let dated = root.join("2026").join("10").join("09").join(format!("{ID_A}.csa"));
        write(&dated, &csa_with_event(ID_A));
        assert_eq!(find_archive_csa(root, ID_A), Some(dated));
    }

    // -- JSONL → CSA --------------------------------------------------------

    const HIRATE: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    const AFTER_76: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
    const AFTER_34: &str = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3";

    const BOARD: &str = "P1-KY-KE-GI-KI-OU-KI-GI-KE-KY\n\
P2 * -HI *  *  *  *  * -KA * \n\
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU\n\
P4 *  *  *  *  *  *  *  *  * \n\
P5 *  *  *  *  *  *  *  *  * \n\
P6 *  *  *  *  *  *  *  *  * \n\
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU\n\
P8 * +KA *  *  *  *  * +HI * \n\
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY\n\
+\n";

    fn meta_line() -> String {
        r#"{"type":"meta","timestamp":"2026-10-09T12:00:00+09:00","settings":{"games":2,"max_moves":320,"byoyomi":0,"btime":300000,"binc":10000,"wtime":300000,"winc":10000,"timeout_margin_ms":1000,"threads":1,"hash_mb":256},"engine_cmd":{"path_black":"a","path_white":"b","label_black":"base","label_white":"test","usi_options_black":[],"usi_options_white":[]},"start_positions":[],"output":"x.jsonl"}"#.to_owned()
    }

    fn move_line(game: u32, ply: u32, side: char, sfen: &str, usi: &str, engine: &str) -> String {
        format!(
            r#"{{"type":"move","game_id":{game},"ply":{ply},"side_to_move":"{side}","sfen_before":"{sfen}","move_usi":"{usi}","engine":"{engine}","elapsed_ms":2500,"think_limit_ms":10000,"timed_out":false}}"#
        )
    }

    fn convert_all(text: &str, only_game: Option<u32>) -> Vec<(u32, Result<String>)> {
        let mut games = Vec::new();
        let count = convert_jsonl(text.as_bytes(), "SPRT-2026-10", only_game, |id, csa| {
            games.push((id, csa));
            Ok(())
        })
        .unwrap();
        assert_eq!(count, games.len());
        games
    }

    /// 2 局入りの `tournament` 形式。1 局目は後手（base）が投了、2 局目は千日手。
    fn tournament_jsonl() -> String {
        let black_eval = r#"{"type":"move","game_id":1,"ply":1,"side_to_move":"b","sfen_before":"SFEN","move_usi":"7g7f","engine":"test","elapsed_ms":2500,"think_limit_ms":10000,"timed_out":false,"eval":{"score_cp":54,"depth":20,"nodes":1000,"pv":["7g7f","3c3d","2g2f"]}}"#.replace("SFEN", HIRATE);
        let white_eval = r#"{"type":"move","game_id":1,"ply":2,"side_to_move":"w","sfen_before":"SFEN","move_usi":"3c3d","engine":"base","elapsed_ms":999,"think_limit_ms":10000,"timed_out":false,"eval":{"score_cp":-30,"pv":["3c3d","2g2f"]}}"#.replace("SFEN", AFTER_76);
        // 手番側視点の詰み評価（先手勝ち）。読み筋の先頭が指し手と違うので評価値だけ残る。
        let mate_eval = r#"{"type":"move","game_id":1,"ply":3,"side_to_move":"b","sfen_before":"SFEN","move_usi":"2g2f","engine":"test","elapsed_ms":61000,"think_limit_ms":10000,"timed_out":false,"eval":{"score_mate":7,"pv":["6i7h"]}}"#.replace("SFEN", AFTER_34);
        let resign = r#"{"type":"move","game_id":1,"ply":4,"side_to_move":"w","sfen_before":"x","move_usi":"resign","engine":"base","elapsed_ms":10,"think_limit_ms":10000,"timed_out":false}"#;
        [
            meta_line(),
            black_eval,
            white_eval,
            mate_eval,
            resign.to_owned(),
            r#"{"type":"result","game_id":1,"outcome":"black_win","reason":"resign","plies":4,"winner":"test","attempt":0}"#.to_owned(),
            move_line(2, 1, 'b', HIRATE, "7g7f", "base"),
            move_line(2, 2, 'w', AFTER_76, "3c3d", "test"),
            r#"{"type":"result","game_id":2,"outcome":"draw","reason":"sennichite","plies":2,"attempt":0}"#.to_owned(),
        ]
        .join("\n")
    }

    #[test]
    fn converts_tournament_jsonl_to_wdoor_csa() {
        let games = convert_all(&tournament_jsonl(), None);
        assert_eq!(games.len(), 2);

        let (id, csa) = &games[0];
        assert_eq!(*id, 1);
        let expected = format!(
            "V2.2\n\
N+test\n\
N-base\n\
$EVENT:SPRT-2026-10\n\
$START_TIME:2026/10/09 12:00:00\n\
$TIME_LIMIT:5:00+10F\n\
{BOARD}\
+7776FU\n\
T2\n\
'** 54 +7776FU -3334FU +2726FU\n\
-3334FU\n\
T0\n\
'** 30 -3334FU +2726FU\n\
+2726FU\n\
T61\n\
'** 100000\n\
%TORYO\n\
'summary:toryo:test win:base lose\n\
'$END_TIME:2026/10/09 12:01:04\n"
        );
        assert_eq!(csa.as_ref().unwrap(), &expected);

        let (id, csa) = &games[1];
        assert_eq!(*id, 2);
        let expected = format!(
            "V2.2\n\
N+base\n\
N-test\n\
$EVENT:SPRT-2026-10\n\
$START_TIME:2026/10/09 12:00:00\n\
$TIME_LIMIT:5:00+10F\n\
{BOARD}\
+7776FU\n\
T2\n\
-3334FU\n\
T2\n\
%SENNICHITE\n\
'summary:sennichite:base draw:test draw\n\
'$END_TIME:2026/10/09 12:00:05\n"
        );
        assert_eq!(csa.as_ref().unwrap(), &expected);
    }

    #[test]
    fn converted_csa_replays_with_the_csa_parser() {
        let games = convert_all(&tournament_jsonl(), Some(1));
        let csa = games[0].1.as_ref().unwrap();
        let (mut pos, moves, info, evals) = rshogi_csa::parse_csa_full_with_evals(csa).unwrap();
        assert_eq!(info.black_name.as_deref(), Some("test"));
        assert_eq!(evals, [Some(54), Some(30), Some(100_000)]);
        for mv in &moves {
            if let rshogi_csa::ParsedMove::Normal(cm) = mv {
                pos.apply_csa_move(&cm.mv).unwrap();
            }
        }
        assert_eq!(
            moves.last(),
            Some(&rshogi_csa::ParsedMove::Special(rshogi_csa::SpecialMove::Resign))
        );
    }

    #[test]
    fn game_id_filter_converts_only_that_game() {
        let games = convert_all(&tournament_jsonl(), Some(2));
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].0, 2);
        assert!(games[0].1.as_ref().unwrap().contains("%SENNICHITE\n"));
        assert!(convert_all(&tournament_jsonl(), Some(9)).is_empty());
    }

    #[test]
    fn converts_csa_client_jsonl_with_one_sided_moves() {
        // csa_client の JSONL: 相手の評価値はなく、自分（後手 RAMU）が 1 手も指す前に
        // 相手が時間切れ。後手名は meta のラベルから補う。
        let meta = r#"{"type":"meta","timestamp":"2026-10-09T00:00:01+09:00","settings":{"games":1,"max_moves":0,"byoyomi":10000,"btime":900000,"binc":0,"timeout_margin_ms":500,"threads":1,"hash_mb":0},"engine_cmd":{"path_black":"remote:Panda","path_white":"engine","label_black":"Panda","label_white":"RAMU","usi_options_black":[],"usi_options_white":[]},"start_positions":[],"output":"x.jsonl"}"#;
        let text = [
            meta.to_owned(),
            move_line(1, 1, 'b', HIRATE, "7g7f", "Panda"),
            r#"{"type":"result","game_id":1,"outcome":"white_win","reason":"time_up","plies":1,"winner":"RAMU"}"#.to_owned(),
        ]
        .join("\n");
        let games = convert_all(&text, None);
        let csa = games[0].1.as_ref().unwrap();
        assert!(csa.contains("N+Panda\nN-RAMU\n"), "{csa}");
        assert!(csa.contains("$TIME_LIMIT:15:00+10\n"), "{csa}");
        assert!(
            csa.ends_with(
                "+7776FU\nT2\n%TIME_UP\n'summary:time up:Panda lose:RAMU win\n'$END_TIME:2026/10/09 00:00:03\n"
            ),
            "{csa}"
        );
    }

    #[test]
    fn end_reason_mapping_covers_tournament_and_csa_client_values() {
        let ok: [(&str, bool, EndReason); 17] = [
            ("resign", false, EndReason::Resign),
            ("win", true, EndReason::WinDeclaration),
            ("win_declaration", false, EndReason::WinDeclaration),
            // csa_client: 理由行なしの決着。
            ("win", false, EndReason::Resign),
            ("lose", false, EndReason::Resign),
            ("timeout", false, EndReason::TimeUp),
            ("time_up", false, EndReason::TimeUp),
            ("illegal_move", false, EndReason::IllegalMove),
            ("sennichite", false, EndReason::Sennichite),
            ("sennichite_perpetual_check", false, EndReason::OuteSennichite),
            ("oute_sennichite", false, EndReason::OuteSennichite),
            ("jishogi", false, EndReason::Jishogi),
            ("max_moves", false, EndReason::MaxMoves),
            ("abnormal", false, EndReason::Abnormal),
            ("adjudication_resign", false, EndReason::Resign),
            ("adjudication_win", false, EndReason::Resign),
            ("resign", true, EndReason::Resign),
        ];
        for (reason, declared, expected) in ok {
            assert_eq!(end_reason_for(reason, declared).unwrap(), expected, "{reason}");
        }
        for reason in [
            "adjudication_draw",
            "no_bestmove",
            "interrupted",
            "cancelled",
            "watchdog",
            "",
        ] {
            assert!(end_reason_for(reason, false).is_err(), "{reason}");
        }
    }

    #[test]
    fn per_game_failures_do_not_stop_other_games() {
        let text = [
            meta_line(),
            move_line(1, 1, 'b', HIRATE, "7g7f", "test"),
            r#"{"type":"result","game_id":1,"outcome":"draw","reason":"error: engine exit","plies":1,"error":true}"#.to_owned(),
            // 手番が飛んでいる（先手の手が 2 回続く）。
            move_line(2, 1, 'b', HIRATE, "7g7f", "test"),
            move_line(2, 2, 'b', AFTER_76, "2g2f", "test"),
            r#"{"type":"result","game_id":2,"outcome":"black_win","reason":"resign","plies":2}"#.to_owned(),
            r#"{"type":"result","game_id":3,"outcome":"black_win","reason":"resign","plies":0}"#.to_owned(),
            move_line(4, 1, 'b', HIRATE, "7g7f", "base"),
            move_line(4, 2, 'w', AFTER_76, "3c3d", "test"),
            r#"{"type":"result","game_id":4,"outcome":"white_win","reason":"timeout","plies":3}"#.to_owned(),
            // result 行のない進行中の局は渡されない。
            move_line(5, 1, 'b', HIRATE, "7g7f", "base"),
        ]
        .join("\n");
        let games = convert_all(&text, None);
        let ok: Vec<(u32, bool)> = games.iter().map(|(id, csa)| (*id, csa.is_ok())).collect();
        assert_eq!(ok, [(1, false), (2, false), (3, false), (4, true)]);
        assert!(games[3].1.as_ref().unwrap().contains("'summary:time up:base lose:test win\n"));
    }

    #[test]
    fn broken_input_aborts_conversion() {
        let no_meta =
            r#"{"type":"result","game_id":1,"outcome":"draw","reason":"max_moves","plies":0}"#;
        assert!(convert_jsonl(no_meta.as_bytes(), "e", None, |_, _| Ok(())).is_err());
        let broken = format!("{}\n{{\"type\":\"move\",", meta_line());
        assert!(convert_jsonl(broken.as_bytes(), "e", None, |_, _| Ok(())).is_err());
    }

    // -- HTTP ---------------------------------------------------------------

    #[derive(Debug, Clone)]
    struct Recorded {
        path: String,
        authorization: Option<String>,
        body: serde_json::Value,
    }

    /// ローカルのモック API サーバー。受けたリクエストを記録し、`respond` の返す
    /// (ステータス, 本文) を返す。
    fn spawn_mock_api(
        respond: impl Fn(&Recorded) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Recorded>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let request = read_request(&stream);
                let (status, body) = respond(&request);
                recorded.lock().unwrap().push(request);
                write_response(stream, status, &body);
            }
        });
        (api_base, requests)
    }

    fn read_request(stream: &TcpStream) -> Recorded {
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut parts = request_line.split_whitespace();
        assert_eq!(parts.next(), Some("POST"));
        let path = parts.next().unwrap().to_owned();
        let mut content_length = 0usize;
        let mut authorization = None;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').unwrap();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap(),
                "authorization" => authorization = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        Recorded {
            path,
            authorization,
            body: serde_json::from_slice(&body).unwrap(),
        }
    }

    fn write_response(mut stream: TcpStream, status: u16, body: &str) {
        let response = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }

    fn publisher(api_base: &str) -> Publisher {
        let api = ApiClient::new(api_base, "secret-token".to_owned()).unwrap();
        Publisher::new(api, Duration::ZERO)
    }

    const ID_NEW: &str = "wdoor+floodgate-300-10F+RAMU+New+20261009000001";
    const ID_KNOWN: &str = "wdoor+floodgate-300-10F+RAMU+Known+20261009003000";
    const ID_GONE: &str = "wdoor+floodgate-300-10F+Old+RAMU_TF+20260706123000";
    const ID_LOST: &str = "wdoor+floodgate-300-10F+Lost+RAMU_TF+20260707000000";
    const ID_BROKEN: &str = "wdoor+floodgate-300-10F+RAMU+Broken+20261009010000";

    #[test]
    fn wdoor_publish_handles_created_existing_archive_fallback_and_failures() {
        let (api_base, requests) = spawn_mock_api(|request| match request.path.as_str() {
            "/api/v1/admin/games/import-wdoor" => {
                match request.body["wdoor_game_id"].as_str().unwrap() {
                    ID_NEW => (
                        200,
                        r#"{"game_id":"wdoor-20261009000001-aaaaaaaaaa","created":true}"#
                            .to_owned(),
                    ),
                    ID_KNOWN => (
                        200,
                        r#"{"game_id":"wdoor-20261009003000-bbbbbbbbbb","created":false}"#
                            .to_owned(),
                    ),
                    ID_GONE | ID_LOST => (404, r#"{"error":"not found on wdoor"}"#.to_owned()),
                    _ => (502, r#"{"error":"wdoor fetch failed"}"#.to_owned()),
                }
            }
            "/api/v1/admin/games/upload" => (
                200,
                r#"{"game_id":"wdoor-20260706123000-cccccccccc","created":true}"#.to_owned(),
            ),
            other => (404, format!(r#"{{"error":"unexpected path {other}"}}"#)),
        });
        let archive = tempfile::tempdir().unwrap();
        let archived =
            archive.path().join("2026").join("07").join("06").join(format!("{ID_GONE}.csa"));
        write(&archived, &csa_with_event(ID_GONE));

        let mut publisher = publisher(&api_base);
        for id in [ID_NEW, ID_KNOWN, ID_GONE, ID_LOST, ID_BROKEN] {
            publisher.publish_wdoor(id, Some(archive.path()));
        }
        assert_eq!(
            publisher.tally,
            Tally {
                created: 2,
                skipped: 1,
                failed: 2,
            }
        );

        let requests = requests.lock().unwrap();
        let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
        let import = "/api/v1/admin/games/import-wdoor";
        // 404 でアーカイブがある局だけ upload へ切り替わる。
        assert_eq!(
            paths,
            [
                import,
                import,
                import,
                "/api/v1/admin/games/upload",
                import,
                import
            ]
        );
        for request in requests.iter() {
            assert_eq!(request.authorization.as_deref(), Some("Bearer secret-token"));
        }
        assert_eq!(
            requests[0].body,
            serde_json::json!({ "wdoor_game_id": ID_NEW, "force": false })
        );
        assert_eq!(
            requests[3].body,
            serde_json::json!({ "csa": csa_with_event(ID_GONE), "source": "wdoor" })
        );
    }

    #[test]
    fn wdoor_404_without_archive_dir_is_a_failure() {
        let (api_base, requests) =
            spawn_mock_api(|_| (404, r#"{"error":"not found on wdoor"}"#.to_owned()));
        let mut publisher = publisher(&api_base);
        publisher.publish_wdoor(ID_GONE, None);
        assert_eq!(
            publisher.tally,
            Tally {
                created: 0,
                skipped: 0,
                failed: 1,
            }
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn archive_csa_with_mismatched_event_is_not_uploaded() {
        let (api_base, requests) =
            spawn_mock_api(|_| (404, r#"{"error":"not found on wdoor"}"#.to_owned()));
        let archive = tempfile::tempdir().unwrap();
        write(&archive.path().join(format!("{ID_GONE}.csa")), &csa_with_event(ID_LOST));
        let mut publisher = publisher(&api_base);
        publisher.publish_wdoor(ID_GONE, Some(archive.path()));
        assert_eq!(publisher.tally.failed, 1);
        assert_eq!(requests.lock().unwrap().len(), 1, "upload は送らない");
    }

    #[test]
    fn csa_upload_sends_event_and_counts_results() {
        let (api_base, requests) =
            spawn_mock_api(|request| match request.body["csa"].as_str().unwrap() {
                "new" => {
                    (200, r#"{"game_id":"upload-0123456789abcdef","created":true}"#.to_owned())
                }
                "same" => {
                    (200, r#"{"game_id":"upload-0123456789abcdef","created":false}"#.to_owned())
                }
                _ => (422, r#"{"error":"illegal move at ply 3"}"#.to_owned()),
            });
        let mut publisher = publisher(&api_base);
        publisher.publish_csa("a.csa", "new", "WCSC36");
        publisher.publish_csa("b.csa", "same", "WCSC36");
        publisher.publish_csa("c.csa", "bad", "WCSC36");
        assert_eq!(
            publisher.tally,
            Tally {
                created: 1,
                skipped: 1,
                failed: 1,
            }
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].path, "/api/v1/admin/games/upload");
        assert_eq!(
            requests[0].body,
            serde_json::json!({ "csa": "new", "event": "WCSC36", "source": "upload" })
        );
    }

    #[test]
    fn api_errors_do_not_leak_the_token() {
        let (api_base, _requests) =
            spawn_mock_api(|_| (401, r#"{"error":"unauthorized"}"#.to_owned()));
        let api = ApiClient::new(&api_base, "secret-token".to_owned()).unwrap();
        let err = api.import_wdoor(ID_NEW).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("401"), "{message}");
        assert!(!message.contains("secret-token"), "{message}");
    }

    #[test]
    fn api_client_rejects_empty_token_and_bad_base() {
        assert!(ApiClient::new("https://example.invalid/api/v1", "  ".to_owned()).is_err());
        assert!(ApiClient::new("example.invalid/api/v1", "t".to_owned()).is_err());
    }
}
