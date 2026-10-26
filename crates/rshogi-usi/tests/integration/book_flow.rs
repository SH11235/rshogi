//! 定跡（opening book）の USI 統合テスト。
//!
//! - YANEURAOU-DB2016 形式の実 `.db` フィクスチャを与えて position→go で bestmove が返ること
//! - BookFile=no_book（既定）では定跡関連の出力が一切無く従来挙動が不変であること
//! - 片側正規化定跡に FlippedBook でヒットすること
//! - 優先定跡 (`<BookFile の名前>-000.db` 等) を BookFile より先に引くこと

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Material 評価で動作させる共通初期化（NNUE ファイル不要）。
const MATERIAL_INIT: &str = "usi\nsetoption name MaterialLevel value 9\n";

/// テスト用の一意な一時ファイルパスを作り、内容を書き込んで返す。
fn write_temp_db(tag: &str, contents: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let unique = format!(
        "rshogi_book_test_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    path.push(unique);
    std::fs::write(&path, contents).expect("write temp .db fixture");
    path
}

/// bestmove を受信してから quit し、探索の自然終了までの stdout を返す。
fn run_engine(input: &str) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || -> std::io::Result<String> {
        let mut output = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            output.push_str(&line);
            output.push('\n');
            if line.starts_with("bestmove ") {
                let _ = sender.send(());
            }
        }
        Ok(output)
    });

    let result = (|| -> std::io::Result<std::process::ExitStatus> {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(input.as_bytes())?;
        receiver.recv_timeout(Duration::from_secs(15)).map_err(std::io::Error::other)?;
        writeln!(stdin, "quit")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "quit timed out"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // 応答待ち・終了待ちの失敗時にも子プロセスと stdout reader を残さない。
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout = reader.join().expect("join stdout reader").expect("read stdout");
    let status = result.unwrap_or_else(|error| panic!("engine session failed: {error}\n{stdout}"));
    assert!(status.success(), "engine exited with failure:\n{stdout}");
    stdout
}

/// `bestmove` 行（最初のもの）を取り出す。
fn first_bestmove(stdout: &str) -> Option<&str> {
    stdout.lines().find(|l| l.starts_with("bestmove"))
}

#[test]
fn book_explore_empty_default_is_advertised_and_disabled() {
    let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let output = cmd
        .write_stdin(format!(
            "{MATERIAL_INIT}\
             setoption name BookExploreFile value <empty>\n\
             isready\n\
             isready\n\
             quit\n"
        ))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(
        stdout
            .lines()
            .any(|line| line == "option name BookExploreFile type string default <empty>"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().filter(|line| *line == "readyok").count(), 2, "{stdout}");
    for output in [&stdout, &stderr] {
        assert!(!output.contains("info string BookExploreFile"), "{output}");
        assert!(!output.contains("book explore loaded"), "{output}");
    }
}

#[test]
fn book_hit_returns_bestmove_from_db() {
    // YANEURAOU-DB2016 形式の実 .db。平手初期局面に 7g7f(ponder 3c3d) を 1 手だけ登録。
    let db = "#YANEURAOU-DB2016 1.00\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
              7g7f 3c3d 30 16 100\n";
    let path = write_temp_db("hit", db);
    let path_str = path.to_string_lossy();

    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         isready\n\
         position startpos\n\
         go depth 10\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    assert!(stdout.contains("book loaded"), "book load info missing:\n{stdout}");
    // 候補が 1 手なので抽選に依らず 7g7f ponder 3c3d が確定する。
    assert_eq!(
        first_bestmove(&stdout),
        Some("bestmove 7g7f ponder 3c3d"),
        "unexpected bestmove:\n{stdout}"
    );
}

#[test]
fn book_on_the_fly_hit_and_unsorted_rejection() {
    let db = "#YANEURAOU-DB2016 1.00\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
              7g7f 3c3d 30 16 100\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1\n\
              3c3d 7g7f 0 16 1\n";
    let unsorted = db.replace(" b - 1\n", " x - 1\n");
    for (tag, contents, expected) in [("otf", db, true), ("otf_unsorted", &unsorted, false)] {
        let path = write_temp_db(tag, contents);
        let path_str = path.to_string_lossy();
        let input = format!(
            "{MATERIAL_INIT}\
             setoption name BookFile value {path_str}\n\
             setoption name BookOnTheFly value true\n\
             isready\n\
             position startpos\n\
             go depth 6\n"
        );
        let stdout = run_engine(&input);
        let _ = std::fs::remove_file(&path);

        assert_eq!(stdout.contains("info string book opened on the fly"), expected, "{stdout}");
        if expected {
            assert_eq!(first_bestmove(&stdout), Some("bestmove 7g7f ponder 3c3d"), "{stdout}");
        } else {
            assert!(stdout.contains("not sorted by SFEN"), "{stdout}");
            assert!(stdout.lines().any(|line| line.starts_with("info depth 6 ")), "{stdout}");
        }
    }
}

#[test]
fn flipped_book_hit_on_one_sided_db() {
    // 片側正規化定跡: 後手番局面のみを flip した先手番の正準局面を登録。
    // 後手番局面 (2 手目、白番) を probe すると flip でヒットする。
    // flipped_key("...w - 2") = 先手番の対称局面。その黒視点の手 7g7f を登録すると、
    // 後手番局面では 3c3d に反転される。
    let flipped_sfen = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 2";
    let db = format!(
        "#YANEURAOU-DB2016 1.00\n\
         sfen {flipped_sfen}\n\
         7g7f 8c8d 20 16 50\n"
    );
    let path = write_temp_db("flip", &db);
    let path_str = path.to_string_lossy();

    // 後手番局面 (先手が 7g7f を指した直後) を position で与える。
    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         isready\n\
         position startpos moves 7g7f\n\
         go depth 10\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    // flip で戻された白の 3c3d(ponder 2g2f)が返る。
    assert_eq!(
        first_bestmove(&stdout),
        Some("bestmove 3c3d ponder 2g2f"),
        "flipped book hit failed:\n{stdout}"
    );
}

#[test]
fn no_book_default_is_unchanged() {
    // BookFile 未設定（既定 no_book）では定跡は一切使われず、通常探索の bestmove が返る。
    let input = format!(
        "{MATERIAL_INIT}\
         isready\n\
         position startpos\n\
         go depth 6\n"
    );
    let stdout = run_engine(&input);

    // 定跡関連の出力が一切無いこと（従来挙動が完全不変）。
    assert!(!stdout.contains("book loaded"), "unexpected book output:\n{stdout}");
    // 通常探索の bestmove が返ること。
    let bm = first_bestmove(&stdout).expect("bestmove present");
    assert!(bm.starts_with("bestmove"), "no bestmove:\n{stdout}");
    // 要求した深さまでの完了を確認し、quit による途中停止を成功扱いしない。
    assert!(stdout.lines().any(|line| line.starts_with("info depth 6 ")), "{stdout}");
}

#[test]
fn own_book_false_falls_back_to_search() {
    // 定跡はロードするが USI_OwnBook=false なら probe をスキップして通常探索する。
    let db = "#YANEURAOU-DB2016 1.00\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
              7g7f 3c3d 30 16 100\n";
    let path = write_temp_db("ownbook", db);
    let path_str = path.to_string_lossy();

    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         setoption name USI_OwnBook value false\n\
         isready\n\
         position startpos\n\
         go depth 6\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    // book のロード通知だけでなく、通常探索が要求した深さまで完了する。
    assert!(stdout.contains("book loaded"), "book should still load:\n{stdout}");
    assert!(stdout.lines().any(|line| line.starts_with("info depth 6 ")), "{stdout}");
    assert!(first_bestmove(&stdout).is_some(), "bestmove present:\n{stdout}");
}

#[test]
fn priority_book_files_are_loaded_in_order_and_hit_first() {
    let hirate = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let dir = std::env::temp_dir().join(format!(
        "rshogi_book_test_priority_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let header = "#YANEURAOU-DB2016 1.00";
    std::fs::write(dir.join("user-000.db"), format!("{header}\nsfen {hirate}\n2g2f 8c8d 0 0 1\n"))
        .unwrap();
    std::fs::write(
        dir.join("user-000.ybb"),
        include_bytes!("../../../rshogi-book/testdata/small.ybb"),
    )
    .unwrap();
    std::fs::write(dir.join("user-001.ybb"), "not a ybb file").unwrap();
    std::fs::write(dir.join("user.db"), format!("{header}\nsfen {hirate}\n7g7f 3c3d 0 0 1\n"))
        .unwrap();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();

    for (on_the_fly, loaded) in [("false", "book loaded"), ("true", "book opened on the fly")] {
        let input = format!(
            "{MATERIAL_INIT}\
             setoption name BookFile value {}\n\
             setoption name BookOnTheFly value {on_the_fly}\n\
             isready\n\
             position startpos\n\
             go depth 6\n",
            path("user.db")
        );
        let stdout = run_engine(&input);
        let book_lines: Vec<&str> = stdout
            .lines()
            .filter(|line| line.contains("book") && !line.starts_with("option "))
            .collect();
        let error = if on_the_fly == "true" {
            "opening"
        } else {
            "loading"
        };
        assert_eq!(book_lines.len(), 4, "{stdout}");
        assert_eq!(
            book_lines[0],
            format!("info string priority book file exists twice. use : {}", path("user-000.db"))
        );
        assert!(
            book_lines[1].starts_with(&format!("info string {loaded}: {} ", path("user-000.db")))
        );
        assert!(book_lines[2].starts_with(&format!(
            "info string Error {error} book file {}: ",
            path("user-001.ybb")
        )));
        assert!(book_lines[3].starts_with(&format!("info string {loaded}: {} ", path("user.db"))));
        assert_eq!(first_bestmove(&stdout), Some("bestmove 2g2f ponder 8c8d"), "{stdout}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn memory_book_reloads_on_next_isready_only_when_a_file_failed() {
    let hirate = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let dir = std::env::temp_dir().join(format!(
        "rshogi_book_test_priority_reload_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let header = "#YANEURAOU-DB2016 1.00";
    std::fs::write(dir.join("user-000.db"), format!("{header}\nsfen {hirate}\n2g2f 8c8d 0 0 1\n"))
        .unwrap();
    std::fs::write(dir.join("user.db"), format!("{header}\nsfen {hirate}\n7g7f 3c3d 0 0 1\n"))
        .unwrap();
    let priority = dir.join("user-000.db").to_string_lossy().into_owned();
    let broken = dir.join("user-001.ybb");

    for (with_broken, expected_loads) in [(false, 1), (true, 2)] {
        if with_broken {
            std::fs::write(&broken, "not a ybb file").unwrap();
        }
        let input = format!(
            "{MATERIAL_INIT}\
             setoption name BookFile value {}\n\
             isready\n\
             isready\n\
             position startpos\n\
             go depth 6\n",
            dir.join("user.db").display()
        );
        let stdout = run_engine(&input);
        let count = |prefix: &str| stdout.lines().filter(|line| line.starts_with(prefix)).count();
        let case = format!("with_broken={with_broken}\n{stdout}");
        assert_eq!(
            count(&format!("info string book loaded: {priority} ")),
            expected_loads,
            "{case}"
        );
        assert_eq!(
            count("info string Error loading book file "),
            if with_broken { 2 } else { 0 },
            "{case}"
        );
        assert_eq!(first_bestmove(&stdout), Some("bestmove 2g2f ponder 8c8d"), "{case}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
