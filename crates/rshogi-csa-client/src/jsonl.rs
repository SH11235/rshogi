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
use crate::record::{GameRecord, JsonlMoveExtra, RecordedMove};

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
    fs::create_dir_all(out_dir).with_context(|| {
        format!("JSONL 出力ディレクトリを作成できません: {}", out_dir.display())
    })?;

    let path = out_dir.join(jsonl_filename(record));
    let file = File::create(&path)
        .with_context(|| format!("JSONL ファイルを作成できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);

    write_meta(&mut writer, record, config, &path)?;
    let plies = write_moves(&mut writer, record)?;
    write_result(&mut writer, record, result, plies)?;

    writer.flush().context("JSONL flush に失敗")?;
    Ok(path)
}

/// `<datetime>_<sente>_vs_<gote>.jsonl` 形式のファイル名を生成する。
fn jsonl_filename(record: &GameRecord) -> String {
    let datetime = record.start_time.format("%Y%m%d_%H%M%S").to_string();
    let sente = sanitize_for_filename(&record.sente_name);
    let gote = sanitize_for_filename(&record.gote_name);
    format!("{datetime}_{sente}_vs_{gote}.jsonl")
}

fn sanitize_for_filename(name: &str) -> String {
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

    // analyze_selfplay は `winner` を `label_black` / `label_white` と照合して集計する。
    // CSA 対局では winner = sente_name または gote_name で送られてくるので、
    // ラベルを CSA 上のプレイヤー名に揃えておくと analyze 側で素直に集計が動く。
    // path_* は自エンジン側に実バイナリパス、相手側に `remote:<name>` を入れる。
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
    let label_black = engine_label_or_fallback(&record.sente_name);
    let label_white = engine_label_or_fallback(&record.gote_name);

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

fn engine_label_or_fallback(name: &str) -> String {
    if name.is_empty() {
        "unknown".to_string()
    } else {
        sanitize_for_filename(name)
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

fn write_moves<W: Write>(writer: &mut W, record: &GameRecord) -> Result<u32> {
    let mut plies: u32 = 0;
    // RecordedMove と JsonlMoveExtra は session.rs で同時に push されるため、
    // 一致しないペアは無視する（CSA から流入した手で extra を持たないものなど）。
    for (idx, (m, extra)) in record.moves.iter().zip(record.jsonl_moves.iter()).enumerate() {
        let ply = (idx as u32) + 1;
        plies = ply;
        let side_char = side_label_char(m.side_to_move);
        let eval = build_eval_log(m, extra);
        let entry = MoveLog {
            kind: "move",
            game_id: 1,
            ply,
            side_to_move: side_char,
            sfen_before: &extra.sfen_before,
            move_usi: &extra.move_usi,
            engine: &extra.engine_label,
            elapsed_ms: extra.elapsed_ms,
            think_limit_ms: extra.think_limit_ms,
            timed_out: false,
            eval,
        };
        serde_json::to_writer(&mut *writer, &entry)?;
        writer.write_all(b"\n")?;
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
    match (result, record.my_color) {
        (GameResult::Win, Color::Black) => Some(record.sente_name.clone()),
        (GameResult::Win, Color::White) => Some(record.gote_name.clone()),
        (GameResult::Lose, Color::Black) => Some(record.gote_name.clone()),
        (GameResult::Lose, Color::White) => Some(record.sente_name.clone()),
        _ => None,
    }
}
