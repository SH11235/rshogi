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
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rshogi_csa::Color;
use serde::Serialize;

use crate::config::CsaClientConfig;
use crate::protocol::GameResult;
use crate::record::{GameRecord, JsonlMoveExtra, RecordStatus, RecordedMove};

/// CSA 経由対局で 1 局分の JSONL を書き出す。
///
/// 既存の `GameRecord` から meta / move / result を組み立てて、`out_dir` 直下に
/// `<datetime>_<sente>_vs_<gote>.jsonl` を作成する。`out_dir` が無ければ作成する。
pub fn write_game_jsonl(
    out_dir: &Path,
    record: &GameRecord,
    config: &CsaClientConfig,
    result: &GameResult,
) -> Result<PathBuf> {
    write_game_jsonl_with_status(out_dir, record, config, result, &RecordStatus::Complete)
}

/// 棋譜種別を明示して CSA 経由対局の JSONL を書き出す。
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

    let path = out_dir.join(jsonl_filename(record, status));
    // live 追記 (`LiveJsonlWriter`) 有効時は同じパスを読者 (kifu_player --live 等) が
    // 開いていることがあるため、truncate 書き込みでなく tmp→rename で置き換え、
    // 読者が常に完全な内容だけを見るようにする。
    let tmp = path.with_extension("jsonl.tmp");
    let file = File::create(&tmp)
        .with_context(|| format!("JSONL ファイルを作成できません: {}", tmp.display()))?;
    let mut writer = BufWriter::new(file);

    write_meta(&mut writer, record, config, &path)?;
    let plies = write_moves(&mut writer, record)?;
    write_result(&mut writer, record, result, plies)?;

    writer.flush().context("JSONL flush に失敗")?;
    rename_with_retry(&tmp, &path)?;
    Ok(path)
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
        let path = out_dir.join(jsonl_filename(record, status));
        let file = File::create(&path)
            .with_context(|| format!("live JSONL を作成できません: {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        write_meta(&mut writer, record, config, &path)?;
        let mut live = Self { writer, written: 0 };
        live.append_new_moves(record)?;
        // 手が 0 の作成直後でも meta 行を読者から見えるようにする
        // (append_new_moves は書くものが無いと flush しない)。
        live.writer.flush().context("live JSONL flush に失敗")?;
        Ok(live)
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

/// `<datetime>_<sente>_vs_<gote>.jsonl` 形式のファイル名を生成する。
fn jsonl_filename(record: &GameRecord, status: &RecordStatus) -> String {
    let datetime = record.start_time.format("%Y%m%d_%H%M%S").to_string();
    let sente = sanitize_for_filename(&record.sente_name);
    let gote = sanitize_for_filename(&record.gote_name);
    format!("{datetime}_{sente}_vs_{gote}{}.jsonl", status.filename_suffix())
}

/// 切断前に作った通常名 live JSONL を削除する。
///
/// 局面不一致 fallback では以後 `_reconnect_fragment.jsonl` へ記録するため、通常名の
/// 未完ファイルを残すと集計側が別対局として拾ってしまう。存在しない場合は成功扱い。
pub(crate) fn remove_complete_live_jsonl(out_dir: &Path, record: &GameRecord) -> Result<()> {
    let path = out_dir.join(jsonl_filename(record, &RecordStatus::Complete));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("切断前 live JSONL を削除できません: {}", path.display())),
    }
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
    let usi_options: Vec<String> = config
        .engine
        .options
        .iter()
        .map(|(k, v)| format!("{k}={}", toml_value_to_string(v)))
        .collect();

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
        // 中断は analyze_selfplay 側で集計対象外にしたいケースもあるが、
        // 現状 outcome=draw としておくと「未決」扱いで wins/losses にカウントされない。
        (GameResult::Interrupted | GameResult::Censored, _) => "draw",
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
        let path = dir.join(jsonl_filename(&record, &RecordStatus::Complete));
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
        let path = dir.join(jsonl_filename(&record, &RecordStatus::Complete));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconnect_fragment_filename_is_explicit() {
        let record = GameRecord::new(&summary());
        let status = RecordStatus::ReconnectFragment {
            reason: "test mismatch".to_owned(),
        };
        assert!(
            jsonl_filename(&record, &status).ends_with("_reconnect_fragment.jsonl"),
            "filename={}",
            jsonl_filename(&record, &status)
        );
    }

    #[test]
    fn fragment_fallback_removes_complete_live_file_only() {
        let dir = std::env::temp_dir().join("csa_live_jsonl_fragment_cleanup_test");
        let _ = std::fs::remove_dir_all(&dir);
        let config = CsaClientConfig::default();
        let record = GameRecord::new(&summary());
        let complete = dir.join(jsonl_filename(&record, &RecordStatus::Complete));
        let fragment_status = RecordStatus::ReconnectFragment {
            reason: "mismatch".to_owned(),
        };
        let fragment = dir.join(jsonl_filename(&record, &fragment_status));

        let _complete_live = LiveJsonlWriter::create(&dir, &record, &config).unwrap();
        let _fragment_live =
            LiveJsonlWriter::create_with_status(&dir, &record, &config, &fragment_status).unwrap();
        assert!(complete.exists());
        assert!(fragment.exists());

        remove_complete_live_jsonl(&dir, &record).unwrap();
        assert!(!complete.exists());
        assert!(fragment.exists());
        // 2 回目も NotFound を成功扱いにする。
        remove_complete_live_jsonl(&dir, &record).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
