//! JSONL 出力モード
//!
//! 対局完了時に `tools::analyze_selfplay` 互換のスキーマで JSONL を 1 ファイル吐く。
//! selfplay (`tools/src/bin/tournament.rs`) の出力と同じ `meta` / `move` / `result`
//! 行構成で、CLI 解析パイプライン（Elo / nElo / 手数分布など）に乗せられる。
//!
//! viewer や Cloudflare R2 への送信は行わない。完全にローカル CLI 解析専用。
//!
//! ## スキーマ
//!
//! - `meta`: `timestamp` / `settings` / `engine_cmd` / `start_positions` / `output`
//! - `move`: `game_id` / `ply` / `side_to_move` / `sfen_before` / `move_usi` /
//!   `engine` / `elapsed_ms` / `think_limit_ms` / `timed_out` / `eval`
//! - `result`: `game_id` / `outcome` / `reason` / `plies` / `winner`
//!
//! `eval` フィールドの構造は selfplay 側の `EvalLog` と同じキー集合を持つ
//! （`score_cp` / `score_mate` / `depth` / `seldepth` / `nodes` / `time_ms` /
//! `nps` / `pv`）。

use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rshogi_csa::Color;
use serde::Serialize;

use crate::config::CsaClientConfig;
use crate::protocol::GameResult;
use crate::record::{GameRecord, JsonlMoveExtra, RecordStatus, RecordedMove, record_filename_base};

/// CSA 経由対局で 1 局分の JSONL を書き出す。
///
/// 既存の `GameRecord` から meta / move / result を組み立てて、`out_dir` 直下に
/// 設定したファイル名に対局識別用 suffix を付けて保存する。ディレクトリは自動作成する。
pub fn write_game_jsonl(
    out_dir: &Path,
    record: &GameRecord,
    config: &CsaClientConfig,
    result: &GameResult,
) -> Result<PathBuf> {
    write_game_jsonl_with_status(out_dir, record, config, result, &RecordStatus::Complete)
}

/// 棋譜種別を明示して CSA 経由対局の JSONL を書き出す。
/// 同局の未完 live JSONL だけを置き換え、完成済み・別内容のファイルは上書きしない。
pub fn write_game_jsonl_with_status(
    out_dir: &Path,
    record: &GameRecord,
    config: &CsaClientConfig,
    result: &GameResult,
    status: &RecordStatus,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir).with_context(|| {
        format!("JSONL 出力ディレクトリを作成できません: {}", out_dir.display())
    })?;

    let path = out_dir.join(jsonl_filename(record, config, status));
    write_jsonl_snapshot(&path, record, config, Some(result))?;
    Ok(path)
}

struct JsonlTemporary {
    path: PathBuf,
    remove_on_drop: bool,
}

impl JsonlTemporary {
    fn reserve(destination: &Path) -> Result<(Self, File)> {
        let path = destination.with_extension("jsonl.tmp");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("JSONL の更新を予約できません: {}", path.display()))?;
        Ok((
            Self {
                path,
                remove_on_drop: true,
            },
            file,
        ))
    }
}

impl Drop for JsonlTemporary {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn write_jsonl_snapshot(
    path: &Path,
    record: &GameRecord,
    config: &CsaClientConfig,
    result: Option<&GameResult>,
) -> Result<usize> {
    // 同名 tmp の create_new が、live 再開と最終保存の更新権を排他する。
    let (mut temporary, file) = JsonlTemporary::reserve(path)?;
    let mut writer = BufWriter::new(file);
    write_meta(&mut writer, record, config, path)?;
    let plies = write_moves(&mut writer, record)?;
    if let Some(result) = result {
        write_result(&mut writer, record, result, plies)?;
    }
    writer.flush().context("JSONL flush に失敗")?;
    drop(writer);

    if let Some(mut existing) = open_owned_live_jsonl(path, record)? {
        let mut expected = File::open(&temporary.path)?;
        existing.rewind()?;
        let mut actual_bytes = [0_u8; 4096];
        let mut expected_bytes = [0_u8; 4096];
        loop {
            let count = existing.read(&mut actual_bytes)?;
            if count == 0 {
                break;
            }
            expected
                .read_exact(&mut expected_bytes[..count])
                .context("既存 live JSONL の方が保存対象より長いため置き換えません")?;
            if actual_bytes[..count] != expected_bytes[..count] {
                bail!("既存 live JSONL の内容が保存対象と一致しません: {}", path.display());
            }
        }
    }
    // rename 後に別 writer が同名 tmp を取得しても、その予約を削除しない。
    temporary.remove_on_drop = false;
    rename_with_retry(&temporary.path, path)?;
    Ok(plies as usize)
}

fn open_owned_live_jsonl(path: &Path, record: &GameRecord) -> Result<Option<File>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let meta: serde_json::Value = serde_json::from_str(&line)
        .with_context(|| format!("既存 JSONL の metadata が不正です: {}", path.display()))?;
    if meta["type"].as_str() != Some("meta")
        || meta["source"].as_str() != Some("csa_client")
        || meta["game_id"].as_str() != Some(&record.game_id)
        || meta["timestamp"].as_str() != Some(record.start_time.to_rfc3339().as_str())
    {
        bail!("既存 JSONL は同じ対局の記録ではありません: {}", path.display());
    }
    if !line.ends_with('\n') {
        bail!("既存 JSONL は行の途中で終わっています: {}", path.display());
    }
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(Some(reader.into_inner()));
        }
        if !line.ends_with('\n') {
            bail!("既存 JSONL は行の途中で終わっています: {}", path.display());
        }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        if row["type"].as_str() == Some("result") {
            bail!("完成済み JSONL は上書きしません: {}", path.display());
        }
    }
}

/// tmp から最終パスへの rename。Windows では宛先を `FILE_SHARE_DELETE` なしで開く
/// 第三者プロセス (ウイルススキャナ・エディタ等) と重なると sharing violation で
/// 失敗しうるため、短いバックオフ付きでリトライする (Unix では通常初回で成功する)。
/// 最終的に失敗した場合は、手動救済できるよう書き出し済み tmp のパスをエラーに含める。
fn rename_with_retry(tmp: &Path, path: &Path) -> Result<()> {
    const ATTEMPTS: u32 = 3;
    const BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(BACKOFF);
        }
        match fs::rename(tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("ATTEMPTS > 0 のため必ず Some")).with_context(|| {
        format!(
            "JSONL の rename に失敗しました: {} (書き出し済みの内容は {} に残っています)",
            path.display(),
            tmp.display()
        )
    })
}

/// 対局中に同スキーマの JSONL を手単位で追記する live ライター
/// (`[record] live_jsonl = true` で session が使う)。meta 行と確定済み move 行のみを
/// 常に完全な行単位で保ち、result 行は書かない。読者 (kifu_player 等) は result 行の
/// 無いファイルを進行中対局として扱う。終局時は [`write_game_jsonl`] が同じパスへ
/// canonical な全内容 (result 行込み) を rename で書き換える。
pub struct LiveJsonlWriter {
    writer: BufWriter<File>,
    /// 書き出し済みの move 行数 (`record.moves` / `jsonl_moves` の消費位置)。
    written: usize,
}

impl LiveJsonlWriter {
    /// 対局開始時 (Game_Summary 確定後) に作る。record が既に持つ手 (途中局面開始・
    /// resume 済みの手) もすべて書き出してから追記を続ける。
    pub fn create(out_dir: &Path, record: &GameRecord, config: &CsaClientConfig) -> Result<Self> {
        Self::create_with_status(out_dir, record, config, &RecordStatus::Complete)
    }

    /// 棋譜種別を明示して live JSONL を作る。
    pub fn create_with_status(
        out_dir: &Path,
        record: &GameRecord,
        config: &CsaClientConfig,
        status: &RecordStatus,
    ) -> Result<Self> {
        fs::create_dir_all(out_dir).with_context(|| {
            format!("JSONL 出力ディレクトリを作成できません: {}", out_dir.display())
        })?;
        let path = out_dir.join(jsonl_filename(record, config, status));
        let written = write_jsonl_snapshot(&path, record, config, None)?;
        let file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("live JSONL を開けません: {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
            written,
        })
    }

    /// record に追加された未書き出しの move 行を追記して flush する。
    pub fn append_new_moves(&mut self, record: &GameRecord) -> Result<()> {
        let upto = record.moves.len().min(record.jsonl_moves.len());
        if self.written >= upto {
            // flush 済み内容から進んでいなければ何もしない (毎手呼ばれる想定)。
            return Ok(());
        }
        let (black, white) = engine_labels(record);
        for idx in self.written..upto {
            let ply = (idx as u32) + 1;
            let m = &record.moves[idx];
            let label = match m.side_to_move {
                Color::Black => &black,
                Color::White => &white,
            };
            write_move_line(&mut self.writer, m, &record.jsonl_moves[idx], ply, label)?;
        }
        self.written = upto;
        self.writer.flush().context("live JSONL flush に失敗")
    }
}

fn jsonl_filename(record: &GameRecord, config: &CsaClientConfig, status: &RecordStatus) -> String {
    format!("{}.jsonl", record_filename_base(record, &config.record, status))
}

/// 切断前に作った通常名 live JSONL を削除する。
///
/// 局面不一致 fallback では以後 `_reconnect_fragment.jsonl` へ記録するため、通常名の
/// 未完ファイルを残すと集計側が別対局として拾ってしまう。存在しない場合は成功扱い。
pub(crate) fn remove_complete_live_jsonl(
    out_dir: &Path,
    record: &GameRecord,
    config: &CsaClientConfig,
) -> Result<()> {
    let path = out_dir.join(jsonl_filename(record, config, &RecordStatus::Complete));
    match fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let (temporary, file) = JsonlTemporary::reserve(&path)?;
    drop(file);
    let existing = open_owned_live_jsonl(&path, record)?;
    drop(existing);
    let result = match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("切断前 live JSONL を削除できません: {}", path.display())),
    };
    drop(temporary);
    result
}

/// ファイル名向けに英数字と `-` `_` 以外を `_` に置換する。空文字は `unknown`。
/// 異なる名前が同じ結果になるため、プレイヤー識別子には使わない。
pub fn sanitize_for_filename(name: &str) -> String {
    if name.is_empty() {
        return "unknown".to_string();
    }
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// meta 行
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct MetaLog<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    source: &'a str,
    game_id: &'a str,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta<'a>,
    start_positions: Vec<String>,
    output: String,
}

#[derive(Serialize)]
struct MetaSettings {
    /// JSONL の利用側（analyze_selfplay）が `total_games_expected` の参照に使う。
    /// CSA 1 対局 = 1 ファイルなので 1 を書く。
    games: u32,
    /// CSA 対局では明確な手数上限はサーバ依存だが、analyze 側は 0 でも問題ない。
    max_moves: u32,
    /// 先手視点の byoyomi (ms)。`Game_Summary` の値をそのまま入れる。
    byoyomi: u64,
    /// 先手視点の持ち時間 (ms)
    btime: u64,
    /// 先手視点の increment (ms)
    binc: u64,
    /// クライアント側の秒読みマージン (ms)
    timeout_margin_ms: u64,
    /// CSA クライアントは USI threads を直接設定しないため 1 を書く。
    /// USI option として渡された場合は engine_cmd.usi_options 側に出る。
    threads: u32,
    /// CSA クライアントは USI_Hash を直接設定しないため 0 を書く。
    /// USI option として渡された場合は engine_cmd.usi_options 側に出る。
    hash_mb: u32,
}

#[derive(Serialize)]
struct EngineCommandMeta<'a> {
    /// 先手側のバイナリパス。自分が先手なら自エンジン、後手なら相手の `Name+` を入れる。
    path_black: String,
    /// 後手側のバイナリパス。
    path_white: String,
    /// 先手側ラベル（analyze_selfplay の集計キー）
    label_black: String,
    /// 後手側ラベル
    label_white: String,
    /// 自エンジンに渡した USI option 文字列（`Name=Value` 形式）
    /// 相手側は不明なので空配列。
    usi_options_black: Vec<String>,
    usi_options_white: Vec<String>,
    /// 対戦相手の名前は不明な相手とローカル側を区別するために残しておく。
    /// `path_*` と冗長だが、analyze_selfplay は無視する追加メタなので OK。
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

fn write_meta<W: Write>(
    writer: &mut W,
    record: &GameRecord,
    config: &CsaClientConfig,
    output_path: &Path,
) -> Result<()> {
    let engine_path = config.engine.path.display().to_string();
    let mut usi_options: Vec<String> = config
        .engine
        .options
        .iter()
        .map(|(k, v)| format!("{k}={}", toml_value_to_string(v)))
        .collect();
    usi_options.sort_unstable();

    // 集計キーはファイル名用の正規化で失われる文字も保持する。
    let (path_black, path_white, options_black, options_white) = match record.my_color {
        Color::Black => (
            engine_path.clone(),
            opponent_descriptor(&record.gote_name),
            usi_options.clone(),
            Vec::new(),
        ),
        Color::White => (
            opponent_descriptor(&record.sente_name),
            engine_path.clone(),
            Vec::new(),
            usi_options.clone(),
        ),
    };
    let (label_black, label_white) = engine_labels(record);

    let initial_sfen = record.initial_position.to_sfen();
    let start_positions = vec![format!("position sfen {initial_sfen}")];

    let meta = MetaLog {
        kind: "meta",
        source: "csa_client",
        game_id: &record.game_id,
        timestamp: record.start_time.to_rfc3339(),
        settings: MetaSettings {
            games: 1,
            max_moves: 0,
            byoyomi: u64_or_zero(record.black_time.byoyomi_ms),
            btime: u64_or_zero(record.black_time.total_time_ms),
            binc: u64_or_zero(record.black_time.increment_ms),
            timeout_margin_ms: config.time.margin_msec,
            threads: 1,
            hash_mb: 0,
        },
        engine_cmd: EngineCommandMeta {
            path_black,
            path_white,
            label_black,
            label_white,
            usi_options_black: options_black,
            usi_options_white: options_white,
            note: Some(
                "csa_client: opponent path is reported as remote name; usi_options are for self-engine only",
            ),
        },
        start_positions,
        output: output_path.display().to_string(),
    };
    serde_json::to_writer(&mut *writer, &meta)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn u64_or_zero(value: i64) -> u64 {
    if value < 0 { 0 } else { value as u64 }
}

fn opponent_descriptor(name: &str) -> String {
    if name.is_empty() {
        "remote:unknown".to_string()
    } else {
        format!("remote:{name}")
    }
}

fn engine_labels(record: &GameRecord) -> (String, String) {
    let label = |name: &str| {
        if name.is_empty() {
            "unknown".to_owned()
        } else {
            name.to_owned()
        }
    };
    let black = label(&record.sente_name);
    let white = label(&record.gote_name);
    if black == white {
        // 同名・空名では名前だけで先後を区別できないため、対局内の識別子を分ける。
        (format!("{black} (black)"), format!("{white} (white)"))
    } else {
        (black, white)
    }
}

fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::String(s) => s.clone(),
        toml::Value::Float(f) => f.to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// move 行
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct MoveLog<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: &'a str,
    move_usi: &'a str,
    engine: &'a str,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    eval: Option<EvalLog>,
}

#[derive(Serialize)]
struct EvalLog {
    #[serde(skip_serializing_if = "Option::is_none")]
    score_cp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score_mate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seldepth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pv: Option<Vec<String>>,
}

fn write_move_line<W: Write>(
    writer: &mut W,
    m: &RecordedMove,
    extra: &JsonlMoveExtra,
    ply: u32,
    engine_label: &str,
) -> Result<()> {
    let entry = MoveLog {
        kind: "move",
        game_id: 1,
        ply,
        side_to_move: side_label_char(m.side_to_move),
        sfen_before: &extra.sfen_before,
        move_usi: &extra.move_usi,
        engine: engine_label,
        elapsed_ms: extra.elapsed_ms,
        think_limit_ms: extra.think_limit_ms,
        timed_out: false,
        eval: build_eval_log(m, extra),
    };
    serde_json::to_writer(&mut *writer, &entry)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_moves<W: Write>(writer: &mut W, record: &GameRecord) -> Result<u32> {
    let mut plies: u32 = 0;
    let (black, white) = engine_labels(record);
    // RecordedMove と JsonlMoveExtra は session.rs で同時に push されるため、
    // 一致しないペアは無視する（CSA から流入した手で extra を持たないものなど）。
    for (idx, (m, extra)) in record.moves.iter().zip(record.jsonl_moves.iter()).enumerate() {
        let ply = (idx as u32) + 1;
        plies = ply;
        let label = match m.side_to_move {
            Color::Black => &black,
            Color::White => &white,
        };
        write_move_line(writer, m, extra, ply, label)?;
    }
    Ok(plies)
}

fn side_label_char(color: Color) -> char {
    match color {
        Color::Black => 'b',
        Color::White => 'w',
    }
}

fn build_eval_log(m: &RecordedMove, extra: &JsonlMoveExtra) -> Option<EvalLog> {
    if m.eval_cp.is_none()
        && m.eval_mate.is_none()
        && m.depth.is_none()
        && extra.seldepth.is_none()
        && extra.nodes.is_none()
        && extra.time_ms.is_none()
        && extra.nps.is_none()
        && m.pv.is_empty()
    {
        return None;
    }
    Some(EvalLog {
        score_cp: m.eval_cp,
        score_mate: m.eval_mate,
        depth: m.depth,
        seldepth: extra.seldepth,
        nodes: extra.nodes,
        time_ms: extra.time_ms,
        nps: extra.nps,
        pv: if m.pv.is_empty() {
            None
        } else {
            Some(m.pv.clone())
        },
    })
}

// ---------------------------------------------------------------------------
// result 行
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ResultLog<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    game_id: u32,
    outcome: &'a str,
    reason: &'a str,
    plies: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<String>,
}

fn write_result<W: Write>(
    writer: &mut W,
    record: &GameRecord,
    result: &GameResult,
    plies: u32,
) -> Result<()> {
    let outcome = outcome_label(result, record.my_color);
    let winner = winner_label(record, result);
    let reason = if record.result.is_empty() {
        outcome
    } else {
        record.result.as_str()
    };
    let entry = ResultLog {
        kind: "result",
        game_id: 1,
        outcome,
        reason,
        plies,
        winner,
    };
    serde_json::to_writer(&mut *writer, &entry)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn outcome_label(result: &GameResult, my_color: Color) -> &'static str {
    match (result, my_color) {
        (GameResult::Draw, _) => "draw",
        (GameResult::Interrupted, _) => "interrupted",
        (GameResult::Censored, _) => "censored",
        (GameResult::Win, Color::Black) => "black_win",
        (GameResult::Win, Color::White) => "white_win",
        (GameResult::Lose, Color::Black) => "white_win",
        (GameResult::Lose, Color::White) => "black_win",
    }
}

fn winner_label(record: &GameRecord, result: &GameResult) -> Option<String> {
    let (black, white) = engine_labels(record);
    match (result, record.my_color) {
        (GameResult::Win, Color::Black) => Some(black),
        (GameResult::Win, Color::White) => Some(white),
        (GameResult::Lose, Color::Black) => Some(white),
        (GameResult::Lose, Color::White) => Some(black),
        _ => None,
    }
}

/// 明示的な source を優先し、source のないログだけを旧 CSA 識別文で判定する。
pub fn is_csa_jsonl(source: Option<&str>, note: Option<&str>) -> bool {
    source == Some("csa_client")
        || (source.is_none()
            && note
                == Some(
                    "csa_client: opponent path is reported as remote name; usi_options are for self-engine only",
                ))
}

/// 明示的な未決結果と、CSA の旧形式で理由から識別できる未決結果を判定する。
/// `draw` / `max_moves` だけが残る旧ログは引分と区別できないため補正しない。
pub fn is_incomplete_result(csa: bool, outcome: &str, reason: Option<&str>) -> bool {
    matches!(outcome, "interrupted" | "censored")
        || (csa && outcome == "draw" && matches!(reason, Some("interrupted" | "censored")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{GameResult, GameSummary, TimeConfig};
    use crate::record::RecordStatus;
    use rshogi_csa::{Color, initial_position};

    fn summary() -> GameSummary {
        GameSummary {
            game_id: "g".to_owned(),
            my_color: Color::Black,
            sente_name: "ME".to_owned(),
            gote_name: "OPP".to_owned(),
            position: initial_position(),
            initial_moves: Vec::new(),
            black_time: TimeConfig::default(),
            white_time: TimeConfig::default(),
            reconnect_token: None,
            entering_king_rule: None,
        }
    }

    fn push_move(record: &mut GameRecord, side: Color, sfen_before: &str, usi: &str) {
        record.add_move("+7776FU", 1, None, side);
        record.add_jsonl_move(JsonlMoveExtra {
            sfen_before: sfen_before.to_owned(),
            move_usi: usi.to_owned(),
            engine_label: "ME".to_owned(),
            elapsed_ms: 100,
            think_limit_ms: 1000,
            seldepth: None,
            nodes: None,
            time_ms: None,
            nps: None,
        });
    }

    #[test]
    fn live_writer_appends_then_final_write_replaces_with_result() {
        let dir = std::env::temp_dir().join("csa_live_jsonl_test");
        let _ = std::fs::remove_dir_all(&dir);
        let config = CsaClientConfig::default();
        let mut record = GameRecord::new(&summary());

        let mut live = LiveJsonlWriter::create(&dir, &record, &config).unwrap();
        let path = dir.join(jsonl_filename(&record, &config, &RecordStatus::Complete));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "作成直後は meta 行のみ");
        assert!(text.contains("\"type\":\"meta\""));

        push_move(&mut record, Color::Black, "sfen1", "7g7f");
        live.append_new_moves(&record).unwrap();
        push_move(&mut record, Color::White, "sfen2", "3c3d");
        live.append_new_moves(&record).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3, "meta + move x2");
        assert!(!text.contains("\"type\":\"result\""), "live 中は result 行なし");

        // 新しい手が無ければ追記しない(毎手呼ばれる想定の冪等性)
        live.append_new_moves(&record).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);

        // 終局: 同じパスへ canonical な全内容 (result 行込み) が rename で置き換わる
        let final_path = write_game_jsonl(&dir, &record, &config, &GameResult::Win).unwrap();
        assert_eq!(final_path, path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 4, "meta + move x2 + result");
        assert!(text.lines().last().unwrap().contains("\"type\":\"result\""));
        assert!(!path.with_extension("jsonl.tmp").exists(), "tmp 残骸なし");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_with_retry_replaces_existing_destination() {
        let dir = std::env::temp_dir().join("csa_jsonl_rename_retry_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("a.jsonl.tmp");
        let dst = dir.join("a.jsonl");
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&dst, "old").unwrap();
        rename_with_retry(&tmp, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new");
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_with_retry_error_mentions_tmp_path() {
        let dir = std::env::temp_dir().join("csa_jsonl_rename_retry_err_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // tmp が存在しない → 全 attempt 失敗。エラーに tmp パスが含まれ手動救済の
        // 手がかりになる。
        let tmp = dir.join("missing.jsonl.tmp");
        let dst = dir.join("a.jsonl");
        let err = rename_with_retry(&tmp, &dst).unwrap_err();
        assert!(format!("{err:#}").contains("missing.jsonl.tmp"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_writer_rewrites_preexisting_moves_on_create() {
        // 途中局面開始・resume では record が既に手を持つ。create がそれらも書き出す。
        let dir = std::env::temp_dir().join("csa_live_jsonl_resume_test");
        let _ = std::fs::remove_dir_all(&dir);
        let config = CsaClientConfig::default();
        let mut record = GameRecord::new(&summary());
        push_move(&mut record, Color::Black, "sfen1", "7g7f");
        push_move(&mut record, Color::White, "sfen2", "3c3d");

        let _live = LiveJsonlWriter::create(&dir, &record, &config).unwrap();
        let path = dir.join(jsonl_filename(&record, &config, &RecordStatus::Complete));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconnect_fragment_filename_is_explicit() {
        let record = GameRecord::new(&summary());
        let config = CsaClientConfig::default();
        let status = RecordStatus::ReconnectFragment {
            reason: "test mismatch".to_owned(),
        };
        assert!(
            jsonl_filename(&record, &config, &status).ends_with("_reconnect_fragment.jsonl"),
            "filename={}",
            jsonl_filename(&record, &config, &status)
        );
    }

    #[test]
    fn fragment_fallback_removes_complete_live_file_only() {
        let dir = std::env::temp_dir().join("csa_live_jsonl_fragment_cleanup_test");
        let _ = std::fs::remove_dir_all(&dir);
        let config = CsaClientConfig::default();
        let record = GameRecord::new(&summary());
        let complete = dir.join(jsonl_filename(&record, &config, &RecordStatus::Complete));
        let fragment_status = RecordStatus::ReconnectFragment {
            reason: "mismatch".to_owned(),
        };
        let fragment = dir.join(jsonl_filename(&record, &config, &fragment_status));

        let _complete_live = LiveJsonlWriter::create(&dir, &record, &config).unwrap();
        let _fragment_live =
            LiveJsonlWriter::create_with_status(&dir, &record, &config, &fragment_status).unwrap();
        assert!(complete.exists());
        assert!(fragment.exists());

        remove_complete_live_jsonl(&dir, &record, &config).unwrap();
        assert!(!complete.exists());
        assert!(fragment.exists());
        // 2 回目も NotFound を成功扱いにする。
        remove_complete_live_jsonl(&dir, &record, &config).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn collision_test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "csa-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scored_record(game_id: &str) -> GameRecord {
        let mut record = GameRecord::new(&summary());
        record.game_id = game_id.to_owned();
        let sfen = record.initial_position.to_sfen();
        push_move(&mut record, Color::Black, &sfen, "7g7f");
        record.moves[0].eval_cp = Some(50);
        record
    }

    #[test]
    fn same_second_games_share_distinct_names_across_all_formats() {
        let dir = collision_test_dir("same-second");
        let mut config = CsaClientConfig::default();
        config.record.dir = dir.clone();
        config.record.filename_template = "constant".to_owned();
        let first = scored_record("first-game");
        let mut second = scored_record("second-game");
        second.start_time = first.start_time;
        second.moves[0].eval_cp = Some(-100);
        let first_json = write_game_jsonl(&dir, &first, &config, &GameResult::Win).unwrap();
        crate::record::save_record(&first, &config.record).unwrap();
        let second_json = write_game_jsonl(&dir, &second, &config, &GameResult::Lose).unwrap();
        crate::record::save_record(&second, &config.record).unwrap();
        assert_ne!(first_json, second_json);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 6);
        for (path, record, score) in [(&first_json, &first, "50"), (&second_json, &second, "-100")]
        {
            assert!(fs::read_to_string(path).unwrap().contains(&record.game_id));
            assert!(
                fs::read_to_string(path.with_extension("csa"))
                    .unwrap()
                    .contains(&record.game_id)
            );
            assert!(fs::read_to_string(path.with_extension("sfen")).unwrap().contains(score));
        }
        let first_csa = fs::read(first_json.with_extension("csa")).unwrap();
        let first_sfen = fs::read(first_json.with_extension("sfen")).unwrap();
        assert!(crate::record::save_record(&first, &config.record).is_err());
        config.record.save_csa = false;
        assert!(crate::record::save_record(&first, &config.record).is_err());
        assert_eq!(fs::read(first_json.with_extension("csa")).unwrap(), first_csa);
        assert_eq!(fs::read(first_json.with_extension("sfen")).unwrap(), first_sfen);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_resume_and_finalization_leave_other_same_second_game_untouched() {
        let dir = collision_test_dir("live-ownership");
        let config = CsaClientConfig::default();
        let first = scored_record("first-game");
        let mut second = scored_record("second-game");
        second.start_time = first.start_time;
        let first_live = LiveJsonlWriter::create(&dir, &first, &config).unwrap();
        let second_live = LiveJsonlWriter::create(&dir, &second, &config).unwrap();
        let first_path = dir.join(jsonl_filename(&first, &config, &RecordStatus::Complete));
        let second_path = dir.join(jsonl_filename(&second, &config, &RecordStatus::Complete));
        let other_before = fs::read(&second_path).unwrap();
        drop(first_live);
        let resumed_live = LiveJsonlWriter::create(&dir, &first, &config).unwrap();
        drop(resumed_live);
        assert_eq!(write_game_jsonl(&dir, &first, &config, &GameResult::Win).unwrap(), first_path);
        let completed = fs::read(&first_path).unwrap();
        assert!(write_game_jsonl(&dir, &first, &config, &GameResult::Lose).is_err());
        assert!(LiveJsonlWriter::create(&dir, &first, &config).is_err());
        assert!(remove_complete_live_jsonl(&dir, &first, &config).is_err());
        assert_eq!(fs::read(&first_path).unwrap(), completed);
        assert_eq!(fs::read(&second_path).unwrap(), other_before);
        drop(second_live);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_initialization_does_not_remove_foreign_file_or_temporary() {
        let dir = collision_test_dir("foreign-file");
        let config = CsaClientConfig::default();
        let record = scored_record("game");
        let path = dir.join(jsonl_filename(&record, &config, &RecordStatus::Complete));
        fs::write(&path, b"foreign file").unwrap();
        assert!(LiveJsonlWriter::create(&dir, &record, &config).is_err());
        assert!(write_game_jsonl(&dir, &record, &config, &GameResult::Win).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign file");
        let temporary = path.with_extension("jsonl.tmp");
        fs::write(&temporary, b"reserved by another writer").unwrap();
        assert!(write_game_jsonl(&dir, &record, &config, &GameResult::Win).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"reserved by another writer");
        assert_eq!(fs::read(&path).unwrap(), b"foreign file");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mismatched_live_game_id_and_start_time_are_preserved() {
        let dir = collision_test_dir("mismatched-identity");
        let config = CsaClientConfig::default();
        let record = scored_record("game");
        let live = LiveJsonlWriter::create(&dir, &record, &config).unwrap();
        drop(live);
        let path = dir.join(jsonl_filename(&record, &config, &RecordStatus::Complete));
        let original = fs::read_to_string(&path).unwrap();
        for (old, new) in [
            ("\"game_id\":\"game\"".to_owned(), "\"game_id\":\"another-game\"".to_owned()),
            (record.start_time.to_rfc3339(), "different-time".to_owned()),
        ] {
            let foreign = original.replace(&old, &new);
            fs::write(&path, &foreign).unwrap();
            assert!(write_game_jsonl(&dir, &record, &config, &GameResult::Win).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), foreign);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn process_writer_fixture() {
        let Some(dir) = std::env::var_os("RSHOGI_RECORD_PROCESS_TEST_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let mut config = CsaClientConfig::default();
        config.record.dir = dir.clone();
        config.record.filename_template = "constant".to_owned();
        let mut record = scored_record("same-game-id");
        record.start_time = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        crate::record::save_record(&record, &config.record).unwrap();
        write_game_jsonl(&dir, &record, &config, &GameResult::Win).unwrap();
    }

    #[test]
    fn parallel_processes_preserve_both_records_with_identical_metadata() {
        let dir = collision_test_dir("parallel-processes");
        let executable = std::env::current_exe().unwrap();
        let spawn = || {
            std::process::Command::new(&executable)
                .args(["--exact", "jsonl::tests::process_writer_fixture"])
                .env("RSHOGI_RECORD_PROCESS_TEST_DIR", &dir)
                .spawn()
                .unwrap()
        };
        let mut first = spawn();
        let mut second = spawn();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 6);
        fs::remove_dir_all(dir).unwrap();
    }
}
