//! JSONL 出力モードの統合テスト。
//!
//! 実 USI / CSA サーバを起動せずに `GameRecord` をプログラムで構築し、
//! `write_game_jsonl` の出力ファイルが analyze_selfplay 互換のスキーマで
//! あることを行ごとに検証する。

use std::path::PathBuf;

use rshogi_csa::{Color, initial_position};
use rshogi_csa_client::config::{CsaClientConfig, EngineConfig, RecordConfig, TimeConfig};
use rshogi_csa_client::jsonl::write_game_jsonl;
use rshogi_csa_client::protocol::{GameResult, TimeConfig as ProtoTimeConfig};
use rshogi_csa_client::record::{GameRecord, JsonlMoveExtra};
use serde_json::Value;

/// テスト用の minimal な `GameRecord` を作る。
fn build_record(my_color: Color) -> GameRecord {
    let pos = initial_position();
    GameRecord {
        game_id: "test-game-1".to_string(),
        sente_name: "alice".to_string(),
        gote_name: "bob".to_string(),
        black_time: ProtoTimeConfig {
            total_time_ms: 60_000,
            byoyomi_ms: 5_000,
            increment_ms: 0,
        },
        white_time: ProtoTimeConfig {
            total_time_ms: 60_000,
            byoyomi_ms: 5_000,
            increment_ms: 0,
        },
        initial_position: pos,
        moves: Vec::new(),
        result: String::new(),
        start_time: chrono::Local::now(),
        my_color,
        jsonl_moves: Vec::new(),
    }
}

fn build_config() -> CsaClientConfig {
    let engine = EngineConfig {
        path: PathBuf::from("/tmp/fake-rshogi-usi"),
        ..Default::default()
    };
    let record = RecordConfig {
        enabled: true,
        ..Default::default()
    };
    CsaClientConfig {
        engine,
        record,
        time: TimeConfig::default(),
        ..Default::default()
    }
}

#[test]
fn writes_meta_move_result_for_self_engine_black() {
    let mut record = build_record(Color::Black);

    // 1 手目（自エンジン = 先手）の指し手を 1 手追加
    let pos = initial_position();
    let sfen_before = pos.to_sfen();
    record.moves.push(rshogi_csa_client::record::RecordedMove {
        csa_move: "+7776FU".to_string(),
        time_sec: 1,
        eval_cp: Some(50),
        eval_mate: None,
        depth: Some(8),
        pv: vec!["7g7f".to_string(), "3c3d".to_string()],
        side_to_move: Color::Black,
    });
    record.jsonl_moves.push(JsonlMoveExtra {
        sfen_before: sfen_before.clone(),
        move_usi: "7g7f".to_string(),
        engine_label: "alice".to_string(),
        elapsed_ms: 1234,
        think_limit_ms: 5_000,
        seldepth: Some(10),
        nodes: Some(12_345),
        time_ms: Some(1_200),
        nps: Some(10_287),
    });
    record.result = "win".to_string();

    let tmp = tempdir();
    let config = build_config();
    let path = write_game_jsonl(&tmp, &record, &config, &GameResult::Win)
        .expect("write_game_jsonl must succeed");

    let lines = std::fs::read_to_string(&path).expect("read jsonl");
    let entries: Vec<Value> = lines
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).expect("parse json line"))
        .collect();
    assert_eq!(entries.len(), 3, "meta + move + result の 3 行");

    // meta
    let meta = &entries[0];
    assert_eq!(meta["type"], "meta");
    assert_eq!(meta["settings"]["games"], 1);
    assert_eq!(meta["engine_cmd"]["label_black"], "alice");
    assert_eq!(meta["engine_cmd"]["label_white"], "bob");
    assert!(meta["engine_cmd"]["path_black"].as_str().unwrap().ends_with("fake-rshogi-usi"));

    // move
    let mv = &entries[1];
    assert_eq!(mv["type"], "move");
    assert_eq!(mv["game_id"], 1);
    assert_eq!(mv["ply"], 1);
    assert_eq!(mv["side_to_move"], "b");
    assert_eq!(mv["sfen_before"], sfen_before);
    assert_eq!(mv["move_usi"], "7g7f");
    assert_eq!(mv["engine"], "alice");
    assert_eq!(mv["elapsed_ms"], 1234);
    assert_eq!(mv["think_limit_ms"], 5000);
    assert_eq!(mv["timed_out"], false);
    assert_eq!(mv["eval"]["depth"], 8);
    assert_eq!(mv["eval"]["seldepth"], 10);
    assert_eq!(mv["eval"]["nodes"], 12345);
    assert_eq!(mv["eval"]["nps"], 10287);
    assert_eq!(mv["eval"]["score_cp"], 50);
    assert_eq!(mv["eval"]["pv"][0], "7g7f");

    // result
    let res = &entries[2];
    assert_eq!(res["type"], "result");
    assert_eq!(res["game_id"], 1);
    assert_eq!(res["outcome"], "black_win");
    assert_eq!(res["plies"], 1);
    assert_eq!(res["winner"], "alice");
}

#[test]
fn writes_white_win_when_self_engine_white_loses() {
    let mut record = build_record(Color::White);
    record.result = "lose".to_string();

    let tmp = tempdir();
    let config = build_config();
    let path = write_game_jsonl(&tmp, &record, &config, &GameResult::Lose)
        .expect("write_game_jsonl must succeed");

    let lines = std::fs::read_to_string(&path).expect("read jsonl");
    let entries: Vec<Value> = lines
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).expect("parse json line"))
        .collect();
    let res = entries.last().unwrap();
    assert_eq!(res["type"], "result");
    // 自エンジン = 白で負け → 黒（相手）勝ち
    assert_eq!(res["outcome"], "black_win");
    assert_eq!(res["winner"], "alice");
}

#[test]
fn draws_when_interrupted_regardless_of_side() {
    let mut record = build_record(Color::Black);
    record.result = "interrupted".to_string();

    let tmp = tempdir();
    let config = build_config();
    let path = write_game_jsonl(&tmp, &record, &config, &GameResult::Interrupted)
        .expect("write_game_jsonl must succeed");

    let lines = std::fs::read_to_string(&path).expect("read jsonl");
    let last = lines.lines().last().unwrap();
    let res: Value = serde_json::from_str(last).unwrap();
    assert_eq!(res["outcome"], "draw");
    assert!(res["winner"].is_null());
}

#[test]
fn filename_includes_datetime_and_player_names() {
    let record = build_record(Color::Black);
    let tmp = tempdir();
    let config = build_config();
    let path = write_game_jsonl(&tmp, &record, &config, &GameResult::Draw).expect("write");
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.ends_with("_alice_vs_bob.jsonl"), "got: {name}");
}

/// テスト用に `target/tmp/csa_client_jsonl_<nanos>` を作って返す簡易 tempdir。
/// 本リポは `tempfile` クレートに依存していないので自前実装する。
fn tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("rshogi-csa-jsonl-test-{nanos}"));
    std::fs::create_dir_all(&path).expect("create tempdir");
    path
}
