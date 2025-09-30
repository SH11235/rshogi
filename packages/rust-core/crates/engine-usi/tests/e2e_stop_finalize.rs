use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn engine_bin() -> std::path::PathBuf {
    // Prefer Cargo-provided path if available.
    // Note: Cargo replaces '-' with '_' in env var names.
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine-usi") {
        return std::path::PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine_usi") {
        return std::path::PathBuf::from(p);
    }
    // Fallbacks to target/debug under workspace root
    let cur = std::env::current_dir().unwrap();
    let mut cand1 = cur.clone();
    cand1.push("target");
    cand1.push("debug");
    #[cfg(windows)]
    cand1.push("engine-usi.exe");
    #[cfg(not(windows))]
    cand1.push("engine-usi");
    if cand1.is_file() {
        return cand1;
    }
    // Try release under current dir target
    let mut cand2 = cur.clone();
    cand2.push("target");
    cand2.push("release");
    #[cfg(windows)]
    cand2.push("engine-usi.exe");
    #[cfg(not(windows))]
    cand2.push("engine-usi");
    if cand2.is_file() {
        return cand2;
    }

    // Try workspace root target via CARGO_MANIFEST_DIR/../../target
    if let Ok(md) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut ws = std::path::PathBuf::from(md);
        // crates/engine-usi -> ../../
        ws.pop();
        ws.pop();
        let mut cand3 = ws.clone();
        cand3.push("target");
        cand3.push("debug");
        #[cfg(windows)]
        cand3.push("engine-usi.exe");
        #[cfg(not(windows))]
        cand3.push("engine-usi");
        if cand3.is_file() {
            return cand3;
        }
        let mut cand4 = ws;
        cand4.push("target");
        cand4.push("release");
        #[cfg(windows)]
        cand4.push("engine-usi.exe");
        #[cfg(not(windows))]
        cand4.push("engine-usi");
        if cand4.is_file() {
            return cand4;
        }
    }

    // Last resort: return a non-existent path; spawn() will error with a clearer message in the test log
    cand2
}

struct UsiProc {
    child: Child,
    tx: ChildStdin,
    rx: Receiver<String>,
}

impl UsiProc {
    fn spawn() -> Self {
        let bin = engine_bin();
        let mut child = Command::new(&bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn engine-usi");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx_lines, rx_lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(s) = line {
                    let _ = tx_lines.send(s);
                } else {
                    break;
                }
            }
        });

        Self {
            child,
            tx: stdin,
            rx: rx_lines,
        }
    }

    fn write_line(&mut self, s: &str) {
        let _ = self.tx.write_all(s.as_bytes());
        let _ = self.tx.write_all(b"\n");
        let _ = self.tx.flush();
    }

    fn wait_for_contains(&self, pat: &str, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => {
                    println!("OUT: {}", line);
                    if line.contains(pat) {
                        return true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        false
    }

    fn assert_no_contains(&self, pat: &str, timeout_ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => {
                    println!("OUT: {}", line);
                    assert!(!line.contains(pat), "unexpected log containing '{}': {}", pat, line);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

impl Drop for UsiProc {
    fn drop(&mut self) {
        // Try to terminate cleanly
        let _ = self.tx.write_all(b"quit\n");
        let _ = self.tx.flush();
        let _ = self.child.wait_timeout(Duration::from_millis(500));
        let _ = self.child.kill();
    }
}

// Add a simple wait_timeout shim for stable
trait WaitTimeout {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        use std::thread;
        let start = Instant::now();
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if Instant::now() - start >= timeout {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}

fn usi_handshake(p: &mut UsiProc) {
    p.write_line("usi");
    assert!(p.wait_for_contains("usiok", 2000), "usiok timeout");
    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000), "readyok timeout");
}

#[test]
#[ignore]
fn e2e_pure_byoyomi_stop_then_gameover_emits_bestmove() {
    if engine_core::util::is_ci_environment() {
        eprintln!("Skipping e2e_pure_byoyomi_stop_then_gameover_emits_bestmove in CI environment (performance-dependent test)");
        return;
    }
    let mut p = UsiProc::spawn();
    usi_handshake(&mut p);
    p.write_line("usinewgame");
    p.write_line("position startpos");
    p.write_line("go btime 0 wtime 0 byoyomi 10000");
    // stop → 直後にgameover（quitはbestmove受信後に送る）
    p.write_line("stop");
    p.write_line("gameover lose");
    assert!(
        p.wait_for_contains("bestmove ", 1000),
        "bestmove was not emitted promptly after stop in pure byoyomi"
    );
    // engine will be dropped and quit in Drop
}

#[test]
#[ignore]
fn e2e_gameover_option_emits_bestmove() {
    let mut p = UsiProc::spawn();
    p.write_line("usi");
    assert!(p.wait_for_contains("usiok", 2000));
    p.write_line("setoption name GameoverSendsBestmove value true");
    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000));
    // 確認: usi再出力で既定値trueになっていること
    p.write_line("usi");
    assert!(
        p.wait_for_contains("option name GameoverSendsBestmove type check default true", 1000),
        "GameoverSendsBestmove was not set to true"
    );
    p.write_line("usinewgame");
    p.write_line("position startpos");
    p.write_line("go btime 0 wtime 0 byoyomi 1000");
    // 少し待ってからgameoverを送る（GUI実運用に近づけ、行レースを避ける）
    std::thread::sleep(Duration::from_millis(50));
    p.write_line("gameover lose");
    assert!(
        p.wait_for_contains("bestmove ", 2000),
        "bestmove was not emitted on gameover with option enabled"
    );
}

#[test]
#[ignore]
fn e2e_stop_fast_finalize_fixed_and_infinite_and_ponder() {
    if engine_core::util::is_ci_environment() {
        eprintln!("Skipping e2e_stop_fast_finalize_fixed_and_infinite_and_ponder in CI environment (performance-dependent test)");
        return;
    }
    // Fixed time
    let mut p1 = UsiProc::spawn();
    usi_handshake(&mut p1);
    p1.write_line("position startpos");
    p1.write_line("go movetime 300");
    p1.write_line("stop");
    assert!(
        p1.wait_for_contains("bestmove ", 1000),
        "bestmove not emitted promptly for movetime"
    );

    // Infinite
    let mut p2 = UsiProc::spawn();
    usi_handshake(&mut p2);
    p2.write_line("position startpos");
    p2.write_line("go infinite");
    p2.write_line("stop");
    assert!(
        p2.wait_for_contains("bestmove ", 1000),
        "bestmove not emitted promptly for infinite"
    );

    // Ponder
    let mut p3 = UsiProc::spawn();
    usi_handshake(&mut p3);
    p3.write_line("setoption name USI_Ponder value true");
    p3.write_line("position startpos");
    p3.write_line("go ponder");
    // Stop should emit bestmove even for ponder
    p3.write_line("stop");
    assert!(
        p3.wait_for_contains("bestmove ", 1000),
        "bestmove not emitted promptly for ponder stop"
    );
}

#[test]
#[ignore]
fn e2e_byoyomi_oob_finalize_logs() {
    let mut p = UsiProc::spawn();
    p.write_line("usi");
    assert!(p.wait_for_contains("usiok", 2000), "usiok timeout");

    // 実戦に近い秒読み設定で TimeManager が near-hard finalize を発火するよう、
    // MinThinkMs とタイムバッファ関連を詰める。
    p.write_line("setoption name Threads value 8");
    p.write_line("setoption name MinThinkMs value 4000");
    p.write_line("setoption name ByoyomiDeadlineLeadMs value 0");
    p.write_line("setoption name ByoyomiSafetyMs value 0");
    p.write_line("setoption name OverheadMs value 0");
    p.write_line("setoption name ByoyomiOverheadMs value 0");
    p.write_line("setoption name StopWaitMs value 0");

    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000), "readyok timeout after option updates");

    p.write_line("usinewgame");
    p.write_line("position startpos");
    p.write_line("go btime 0 wtime 0 byoyomi 2000");

    assert!(
        p.wait_for_contains("oob_finalize_request", 5000),
        "OOB finalize log was not observed"
    );
    assert!(
        p.wait_for_contains("bestmove ", 2000),
        "bestmove was not emitted after OOB finalize"
    );
}

#[test]
#[ignore]
fn e2e_ponder_stop_then_go_fast() {
    let mut p = UsiProc::spawn();
    usi_handshake(&mut p);
    p.write_line("setoption name Threads value 4");
    p.write_line("setoption name StopWaitMs value 100");
    p.write_line("setoption name USI_Ponder value true");
    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000));

    for _ in 0..5 {
        p.write_line("usinewgame");
        p.write_line("position startpos");
        p.write_line("go btime 0 wtime 0 byoyomi 2000");
        assert!(p.wait_for_contains("bestmove ", 4000));

        p.write_line("position startpos moves 7g7f 3c3d");
        p.write_line("go ponder btime 0 wtime 0 byoyomi 2000");
        p.assert_no_contains("fallback_deadline_trigger=", 300);
        p.write_line("stop");
        assert!(p.wait_for_contains("bestmove ", 2000));

        p.write_line("position startpos moves 7g7f 3c3d 2g2f");
        p.write_line("go btime 0 wtime 0 byoyomi 2000");
        assert!(p.wait_for_contains("oob_session_start", 1000));
        assert!(p.wait_for_contains("bestmove ", 4000));
    }
}
