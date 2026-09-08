#![cfg(unix)]

use std::fs;
use std::path::Path;

use rshogi_core::position::Position;
use rshogi_core::types::{Color, RepetitionState};
use tools::selfplay::game::{GameConfig, run_game};
use tools::selfplay::position::{build_position, parse_position_line};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::{EngineConfig, EngineProcess};

fn engine(script: &Path, log: &Path, replies: &Path, label: &str) -> EngineProcess {
    EngineProcess::spawn(
        &EngineConfig {
            path: "/bin/sh".into(),
            args: [script, log, replies].map(|p| p.display().to_string()).to_vec(),
            threads: 1,
            hash_mb: 1,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: Vec::new(),
        },
        label.to_string(),
    )
    .unwrap()
}

// 送信された基点・初期 pass 権・moves を、エンジンと同じ順序で再生する。
fn replay(command: &str) -> Position {
    let (base, moves) = command.split_once(" moves ").unwrap_or((command, ""));
    let (base, rights) = if let Some((base, rights)) = base.split_once(" passrights ") {
        let mut rights = rights.split_whitespace().map(|s| s.parse::<u8>().unwrap());
        (base, Some((rights.next().unwrap(), rights.next().unwrap())))
    } else {
        (base, None)
    };
    let mut parsed = parse_position_line(base).unwrap();
    parsed.moves = moves.split_whitespace().map(str::to_string).collect();
    build_position(&parsed, rights.map(|r| r.0), rights.map(|r| r.1)).unwrap()
}

fn run_case(start: &str, rights: Option<(u8, u8)>, replies: &[&str], reason: &str) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("engine.sh");
    let log = dir.path().join("commands.log");
    let reply_file = dir.path().join("replies.txt");
    fs::write(&reply_file, replies.join("\n") + "\n").unwrap();
    fs::write(
        &script,
        r#"log=$1
replies=$2
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name history-test\nusiok\n' ;;
    isready) printf 'readyok\n' ;;
    position*)
      printf '%s\n' "$line" >> "$log"
      count=0
      in_moves=0
      for token in $line; do
        if [ "$in_moves" = 1 ]; then count=$((count + 1)); fi
        if [ "$token" = moves ]; then in_moves=1; fi
      done
      ;;
    go*)
      move=$(sed -n "$((count + 1))p" "$replies")
      printf 'bestmove %s\n' "${move:-resign}"
      ;;
    quit) break ;;
  esac
done
"#,
    )
    .unwrap();
    let start = parse_position_line(start).unwrap();
    let initial = build_position(&start, rights.map(|r| r.0), rights.map(|r| r.1)).unwrap();
    let mut black = engine(&script, &log, &reply_file, "black");
    let mut white = engine(&script, &log, &reply_file, "white");
    let config = GameConfig {
        resign_rule: None,
        draw_rule: None,
        max_moves: replies.len() as u32,
        timeout_margin_ms: 1000,
        pass_rights: rights,
        go_depth: Some(1),
        go_nodes_black: None,
        go_nodes_white: None,
    };
    // 同じプロセスを再利用し色も交換する。2局目に1局目の手順が残らないことを確認。
    for game_id in 0..2 {
        black.new_game().unwrap();
        white.new_game().unwrap();
        fs::write(&log, "").unwrap();
        let mut events = Vec::new();
        let result = run_game(
            &mut black,
            &mut white,
            &start,
            TimeControl::new(0, 0, 0, 0, 0),
            &config,
            game_id,
            &mut |event| {
                events.push((
                    event.sfen_before.clone(),
                    event.move_usi.clone(),
                    event.raw_move_usi.clone(),
                ));
            },
            None,
        )
        .unwrap();
        assert_eq!(result.reason, reason);
        assert_eq!(result.plies, replies.len() as u32);
        let commands = fs::read_to_string(&log).unwrap();
        let commands: Vec<_> = commands.lines().collect();
        assert_eq!(commands.len(), events.len());
        assert!(!commands[0].contains(" moves "));
        assert_eq!(replay(commands[0]).to_sfen(), initial.to_sfen());
        for (ply, (command, (sfen, _, _))) in commands.iter().zip(&events).enumerate() {
            let replayed = replay(command);
            assert_eq!(replayed.to_sfen(), *sfen, "ply={ply}: {command}");
            let sent_moves = command.split_once(" moves ").map(|(_, m)| m).unwrap_or("");
            let expected_moves: Vec<_> = events[..ply].iter().map(|(_, m, _)| m.as_str()).collect();
            assert_eq!(sent_moves, expected_moves.join(" "));
            if rights.is_some() {
                for color in [Color::Black, Color::White] {
                    let consumed = events[..ply]
                        .iter()
                        .enumerate()
                        .filter(|(i, (_, m, _))| {
                            m == "pass"
                                && (if i % 2 == 0 {
                                    initial.side_to_move()
                                } else {
                                    !initial.side_to_move()
                                }) == color
                        })
                        .count() as u8;
                    assert_eq!(replayed.pass_rights(color), initial.pass_rights(color) - consumed);
                }
            }
            if reason == "sennichite" && ply == 4 {
                // 同じ盤でも snapshot では失われる反復情報が、moves 再生で復元される。
                assert_eq!(replayed.repetition_state(100), RepetitionState::Draw);
                let mut snapshot = Position::new();
                snapshot.set_sfen(sfen).unwrap();
                assert_eq!(snapshot.repetition_state(100), RepetitionState::None);
            }
        }
        for (reply, (_, canonical, raw)) in replies.iter().zip(&events) {
            if *reply == "0000" {
                assert_eq!(canonical, "pass");
                assert_eq!(raw.as_deref(), Some("0000"));
            }
        }
        std::mem::swap(&mut black, &mut white);
    }
}

#[test]
fn history_reconstructs_repetition_and_resets_between_games() {
    run_case(
        "startpos",
        None,
        &[
            "3i4h", "7a6b", "4h3i", "6b7a", "3i4h", "7a6b", "4h3i", "6b7a", "3i4h", "7a6b", "4h3i",
            "6b7a",
        ],
        "sennichite",
    );
}

#[test]
fn opening_moves_are_applied_once_before_history_starts() {
    run_case("startpos moves 7g7f", None, &["3c3d", "2g2f", "resign"], "resign");
    run_case(
        "sfen 4k4/9/9/9/9/9/9/9/4K4 w R 37 moves 5a4a",
        None,
        &["R*5e", "4a3a", "5e5c+", "resign"],
        "resign",
    );
}

#[test]
fn pass_history_uses_rights_at_game_start_and_normalizes_alias() {
    run_case(
        "startpos moves pass",
        Some((2, 2)),
        &["0000", "pass", "pass", "resign"],
        "resign",
    );
}

#[test]
fn illegal_and_special_responses_are_not_sent_as_moves() {
    run_case("startpos", None, &["7g7f", "7g7f"], "illegal_move");
    run_case("startpos", None, &["7g7f", "win"], "win");
    run_case("startpos", None, &["R*5e"], "illegal_move");
    run_case("startpos", None, &["none"], "illegal_move");
    run_case("sfen 4k4/9/9/9/9/9/9/9/4K4 b P 1", None, &["P*4a"], "illegal_move");
}
