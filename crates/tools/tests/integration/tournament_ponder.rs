#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::Value;

const ENGINE: &str = r#"#!/usr/bin/env python3
import json, sys, time
log = open(LOG_PATH, 'a', buffering=1)
mode = MODE
moves = []
pending = False

def reply():
    global pending
    pending = False
    cycle = ['3i4h', '7a6b', '4h3i', '6b7a']
    move = cycle[len(moves) % 4]
    prediction = cycle[(len(moves) + 1) % 4]
    if mode == 'bad_prediction':
        prediction = 'resign'
    if mode == 'illegal_prediction':
        prediction = '5a5c'
    if mode in ('miss', 'early_miss'):
        prediction = '3c3d' if len(moves) % 2 == 0 else '7g7f'
    if mode in ('resign', 'win') and len(moves) >= 1:
        move = mode
    if mode == 'illegal' and len(moves) >= 1:
        move = '9i9h'
    suffix = '' if mode == 'none' else ' ponder ' + prediction
    print('bestmove ' + move + suffix, flush=True)

for raw in sys.stdin:
    line = raw.strip()
    log.write(line + '\n')
    if line == 'usi':
        print('option name Stochastic_Ponder type check default false', flush=True)
        if mode == 'advertised':
            print('option name USI_Ponder type check default false', flush=True)
            print('option name Ponder type check default false', flush=True)
        print('usiok', flush=True)
    elif line == 'isready':
        print('readyok', flush=True)
    elif line == 'usinewgame':
        assert not pending, 'ponder leaked into next game'
        moves = []
    elif line.startswith('position '):
        assert not pending, 'position before ponder drain'
        tokens = line.split()
        moves = tokens[tokens.index('moves') + 1:] if 'moves' in tokens else []
    elif line.startswith('go ponder'):
        assert not pending
        pending = True
        if mode in ('early_hit', 'early_miss'):
            reply()
    elif line == 'ponderhit':
        if mode in ('early_hit', 'early_miss'):
            assert not pending
            continue
        assert pending, 'ponderhit without go ponder'
        if mode not in ('timeout', 'undrained') or len(moves) < 1:
            reply()
    elif line.startswith('go'):
        assert not pending
        if mode == 'slow':
            time.sleep(0.5)
        if mode in ('timeout', 'undrained') and len(moves) >= 1:
            pending = True
        else:
            reply()
    elif line == 'stop':
        if mode == 'undrained': continue
        if mode in ('early_hit', 'early_miss') and not pending: continue
        assert pending, 'stop without outstanding search'
        time.sleep(0.01)
        pending = False
        print('bestmove 9i9h', flush=True)
    elif line == 'quit':
        assert not pending or mode == 'undrained', 'quit before ponder drain'
        break
"#;

struct Run {
    dir: tempfile::TempDir,
    rows: Vec<Value>,
    meta: Value,
    logs: [Vec<String>; 2],
    stdout: String,
}

fn run(modes: [&str; 2], ponder: &[&str], extra: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tournament"));
    for (i, mode) in modes.into_iter().enumerate() {
        let script = dir.path().join(format!("engine-{i}.py"));
        let log = dir.path().join(format!("commands-{i}"));
        fs::write(
            &script,
            ENGINE
                .replace("LOG_PATH", &serde_json::to_string(&log).unwrap())
                .replace("MODE", &serde_json::to_string(mode).unwrap()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        cmd.arg("--engine").arg(script).arg("--engine-label").arg(format!("e{i}"));
    }
    for value in ponder {
        cmd.args(["--ponder", value]);
    }
    let out = dir.path().join("out");
    if !extra.contains(&"--max-moves") {
        cmd.args(["--max-moves", "5"]);
    }
    let output = cmd
        .args(["--games", "2", "--nodes", "1", "--seed", "42"])
        .args(extra)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let rows = fs::read_to_string(out.join("pair-0-1__e0-vs-e1.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let meta: Value = serde_json::from_slice(&fs::read(out.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["invalid"], false, "{meta}");
    let logs = std::array::from_fn(|i| {
        fs::read_to_string(dir.path().join(format!("commands-{i}")))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    });
    Run {
        dir,
        rows,
        meta,
        logs,
        stdout: String::from_utf8(output.stdout).unwrap(),
    }
}

fn results(run: &Run) -> Vec<&Value> {
    run.rows.iter().filter(|row| row["type"] == "result").collect()
}

#[test]
fn ponder_hit_miss_stochastic_and_no_prediction_command_sequences() {
    for (mode, extra, hits, sent) in [
        ("hit", vec![], 2, 2),
        ("miss", vec![], 0, 2),
        ("hit", vec!["--engine-usi-option", "0:Stochastic_Ponder=true"], 2, 2),
        ("none", vec![], 0, 0),
        ("bad_prediction", vec![], 0, 0),
        ("illegal_prediction", vec![], 0, 0),
        ("none", vec!["--engine-usi-option", "0:Stochastic_Ponder=true"], 0, 0),
    ] {
        let run = run([mode, "hit"], &["0:true", "1:false"], &extra);
        assert_eq!(results(&run).len(), 4);
        for result in results(&run) {
            assert_eq!(result["reason"], "max_moves");
            assert_eq!(result["plies"], 5);
            let color = usize::from(result["game_id"].as_u64().unwrap() % 2 == 0);
            assert_eq!(
                result["ponder"][color]["sent"],
                if sent > 0 { sent - color as u64 } else { 0 },
                "{mode}: {result}"
            );
            assert_eq!(
                result["ponder"][color]["hits"],
                if hits > 0 { hits - color as u64 } else { 0 }
            );
            assert_eq!(result["ponder"][1 - color]["sent"], 0);
            let stat = &result["ponder"][color];
            if sent == 0 {
                assert!(stat["hit_rate"].is_null());
            } else {
                assert_eq!(
                    stat["hit_rate"].as_f64().unwrap(),
                    stat["hits"].as_f64().unwrap() / stat["sent"].as_f64().unwrap()
                );
            }
        }
        assert_eq!(run.meta["ponder"][0]["sent"], if sent > 0 { (sent * 2 - 1) * 2 } else { 0 });
        assert_eq!(run.meta["ponder"][0]["hits"], if hits > 0 { (hits * 2 - 1) * 2 } else { 0 });
        assert_eq!(run.meta["engines"][0]["ponder"], true);
        assert!(!run.stdout.contains("Ponder e1:"));
        assert!(run.meta["engines"][1].get("ponder").is_none());
        assert!(!run.logs[1].iter().any(|s| s.starts_with("go ponder") || s == "ponderhit"));
        let commands = &run.logs[0];
        let ponder_indices: Vec<_> = commands
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.starts_with("go ponder").then_some(i))
            .collect();
        assert_eq!(
            ponder_indices.len(),
            if sent > 0 {
                (sent as usize * 2 - 1) * 2
            } else {
                0
            }
        );
        let mut lengths = Vec::new();
        for i in ponder_indices {
            assert_eq!(commands[i], "go ponder nodes 1");
            assert!(commands[i - 1].starts_with("position sfen "));
            let moves: Vec<_> =
                commands[i - 1].split(" moves ").nth(1).unwrap().split_whitespace().collect();
            lengths.push(moves.len());
            let cycle = ["3i4h", "7a6b", "4h3i", "6b7a"];
            let actual = if mode == "miss" {
                &moves[..moves.len() - 1]
            } else {
                &moves[..]
            };
            assert!(actual.iter().enumerate().all(|(i, mv)| *mv == cycle[i % 4]));
            assert!(commands[i + 1] == "ponderhit" || commands[i + 1] == "stop");
            if commands[i + 1] == "ponderhit" {
                assert!(
                    commands[i + 2].starts_with("position ")
                        || commands[i + 2] == "usinewgame"
                        || commands[i + 2] == "quit"
                );
            } else if commands[i + 2].starts_with("position ") {
                assert_eq!(commands[i + 3], "go nodes 1");
            } else {
                assert!(matches!(commands[i + 2].as_str(), "usinewgame" | "quit"));
            }
        }
        lengths.sort_unstable();
        if sent > 0 {
            assert_eq!(lengths, vec![2, 2, 3, 3, 4, 4]);
        }
        #[cfg(feature = "csa-replay")]
        {
            use tools::replay::{GameSource, JsonlSource};
            let source = JsonlSource::new(run.dir.path().join("out"));
            let index = source.build_index().unwrap();
            assert_eq!(index.entries.len(), 4);
            for entry in &index.entries {
                source.load_game(&index, entry).unwrap();
            }
        }
        let output = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
            .arg(run.dir.path().join("out/pair-0-1__e0-vs-e1.jsonl"))
            .arg("--json")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn stochastic_option_preserves_ponder_commands_and_statistics() {
    for mode in [
        "hit",
        "miss",
        "none",
        "bad_prediction",
        "illegal_prediction",
    ] {
        let baseline = run([mode, "hit"], &["0:true", "1:false"], &[]);
        for extra in [
            ["--engine-usi-option", "0:Stochastic_Ponder=true"],
            ["--usi-option", "Stochastic_Ponder=true"],
        ] {
            let configured = run([mode, "hit"], &["0:true", "1:false"], &extra);
            assert!(
                configured.logs[0]
                    .iter()
                    .any(|s| s == "setoption name Stochastic_Ponder value true")
            );
            for (actual, expected) in configured.logs.iter().zip(&baseline.logs) {
                let commands: Vec<_> = actual
                    .iter()
                    .filter(|s| !s.starts_with("setoption name Stochastic_Ponder "))
                    .collect();
                assert_eq!(commands, expected.iter().collect::<Vec<_>>(), "{mode}: {extra:?}");
            }
            assert_eq!(configured.meta["ponder"], baseline.meta["ponder"], "{mode}");
            for (actual, expected) in results(&configured).iter().zip(results(&baseline)) {
                assert_eq!(actual["ponder"], expected["ponder"], "{mode}");
            }
        }
    }
}

#[test]
fn disabled_ponder_preserves_commands_and_omits_output_fields() {
    let run = run(["hit", "hit"], &[], &[]);
    assert!(run.meta.get("ponder").is_none());
    for result in results(&run) {
        assert!(result.get("ponder").is_none());
    }
    let initial = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let positions = [
        format!("position sfen {initial}"),
        format!("position sfen {initial} moves 3i4h"),
        format!("position sfen {initial} moves 3i4h 7a6b"),
        format!("position sfen {initial} moves 3i4h 7a6b 4h3i"),
        format!("position sfen {initial} moves 3i4h 7a6b 4h3i 6b7a"),
    ];
    for (index, commands) in run.logs.iter().enumerate() {
        let mut expected: Vec<String> = [
            "usi",
            "isready",
            "usinewgame",
            "quit",
            "usi",
            "isready",
            "usinewgame",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for game in 0..4 {
            expected.extend(["usinewgame".into(), "isready".into()]);
            for (ply, position) in positions.iter().enumerate() {
                if ply % 2 == (game + index) % 2 {
                    expected.extend([position.clone(), "go nodes 1".into()]);
                }
            }
        }
        expected.push("quit".into());
        assert_eq!(*commands, expected);
    }
}

#[test]
fn terminal_opponent_drains_ponder_before_next_game() {
    for (mode, extra, reason) in [
        ("resign", vec![], "resign"),
        ("win", vec![], "win"),
        ("illegal", vec![], "illegal_move"),
        ("timeout", vec!["--byoyomi", "2000", "--timeout-margin-ms", "500"], "timeout"),
        ("hit", vec!["--max-moves", "16"], "sennichite"),
    ] {
        let run = run(["hit", mode], &["0:true", "1:true"], &extra);
        for result in results(&run) {
            assert_eq!(result["reason"], reason, "{result}");
        }
        assert!(run.logs[0].iter().any(|s| s == "stop"));
        for commands in &run.logs {
            assert!(commands.windows(2).all(|pair| pair[0] != "stop"
                || pair[1] == "usinewgame"
                || pair[1] == "quit"
                || pair[1].starts_with("position ")));
        }
    }
}

#[test]
fn pondering_time_is_excluded_from_move_clock() {
    for (mode, stochastic) in [
        ("hit", false),
        ("hit", true),
        ("miss", false),
        ("miss", true),
    ] {
        let mut extra = vec!["--byoyomi", "10000"];
        if stochastic {
            extra.extend(["--engine-usi-option", "0:Stochastic_Ponder=true"]);
        }
        let run = run([mode, "slow"], &["0:true"], &extra);
        let engine_moves: Vec<_> =
            run.rows.iter().filter(|r| r["type"] == "move" && r["engine"] == "e0").collect();
        assert!(engine_moves.len() >= 8);
        let ponder_time: u64 = run
            .rows
            .iter()
            .filter(|r| r["type"] == "move" && r["engine"] == "e1")
            .map(|r| r["elapsed_ms"].as_u64().unwrap())
            .sum();
        let own_time: u64 = engine_moves.iter().map(|r| r["elapsed_ms"].as_u64().unwrap()).sum();
        assert!(own_time < ponder_time, "own={own_time}, opponent={ponder_time}");
        for row in engine_moves {
            let elapsed = row["elapsed_ms"].as_u64().unwrap();
            if mode == "miss" && row["ply"].as_u64().unwrap() >= 3 {
                assert!(elapsed >= 8, "{row}");
            }
            assert_eq!(row["timed_out"], false);
        }
    }
}

#[test]
fn undrained_timeout_keeps_loss_and_replaces_worker() {
    let run = run(
        ["hit", "undrained"],
        &["0:true", "1:true"],
        &["--byoyomi", "2000", "--timeout-margin-ms", "500"],
    );
    assert_eq!(results(&run).len(), 4);
    for result in results(&run) {
        assert_eq!(result["reason"], "timeout", "{result}");
        assert_ne!(result["outcome"], "draw", "{result}");
    }
    for commands in &run.logs {
        let spawns = commands.iter().filter(|line| *line == "usi").count();
        assert!(spawns >= 4);
        assert_eq!(commands.iter().filter(|line| *line == "usinewgame").count(), spawns + 4);
        assert!(
            commands.split(|line| line == "usi").all(|session| session
                .iter()
                .filter(|line| *line == "usinewgame")
                .count()
                <= 2)
        );
    }
}

#[test]
fn contradictory_usi_ponder_is_rejected_before_spawning() {
    for (ponder, option) in [
        ("0:true", "USI_Ponder=FaLsE"),
        ("0:true", "Ponder=false"),
        ("0:false", "USI_Ponder=TrUe"),
        ("0:false", "Ponder=true"),
    ] {
        for flag in ["--usi-option", "--engine-usi-option"] {
            let dir = tempfile::tempdir().unwrap();
            let value = if flag == "--engine-usi-option" {
                format!("0:{option}")
            } else {
                option.to_owned()
            };
            let output = Command::new(env!("CARGO_BIN_EXE_tournament"))
                .args([
                    "--engine",
                    "/bin/true",
                    "--engine",
                    "/bin/true",
                    "--engine-label",
                    "a",
                    "--engine-label",
                    "b",
                    "--ponder",
                    ponder,
                    flag,
                    &value,
                    "--nodes",
                    "1",
                    "--games",
                    "2",
                    "--out-dir",
                ])
                .arg(dir.path())
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("conflicts with --ponder"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn early_bestmove_is_drained_on_hits_and_misses_across_games() {
    for mode in ["early_hit", "early_miss"] {
        let run = run([mode, "hit"], &["0:true"], &[]);
        assert_eq!(results(&run).len(), 4);
        for result in results(&run) {
            assert_eq!(result["reason"], "max_moves", "{result}");
        }
        for row in run.rows.iter().filter(|row| row["type"] == "move") {
            assert_eq!(row["timed_out"], false, "{row}");
        }
        assert!(run.logs[0].iter().any(|line| line.starts_with("go ponder")));
        let expected = if mode == "early_hit" {
            "ponderhit"
        } else {
            "stop"
        };
        assert!(run.logs[0].iter().any(|line| line == expected));
    }
}

#[test]
fn usi_ponder_without_explicit_flag_and_case_insensitive_values_are_accepted() {
    for (ponder, value) in [
        (vec![], "true"),
        (vec!["0:true"], "TrUe"),
        (vec!["0:false"], "FaLsE"),
    ] {
        for name in ["USI_Ponder", "Ponder"] {
            let option = format!("{name}={value}");
            let indexed = format!("0:{option}");
            for extra in [
                ["--usi-option", option.as_str()],
                ["--engine-usi-option", indexed.as_str()],
            ] {
                let run = run(["advertised", "hit"], &ponder, &extra);
                assert!(
                    run.logs[0]
                        .iter()
                        .any(|line| line == &format!("setoption name {name} value {value}"))
                );
                assert_eq!(results(&run).len(), 4);
            }
        }
    }
}
