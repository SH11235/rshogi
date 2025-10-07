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
        self.wait_for(timeout_ms, |line| line.contains(pat))
    }

    fn wait_for<F>(&self, timeout_ms: u64, mut on_line: F) -> bool
    where
        F: FnMut(&str) -> bool,
    {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => {
                    println!("OUT: {}", line);
                    if on_line(&line) {
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
    if engine_core::util::is_ci_environment() {
        eprintln!("Skipping e2e_gameover_option_emits_bestmove in CI environment (performance-dependent test)");
        return;
    }
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
    p1.assert_no_contains("bestmove ", 200);

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
    if engine_core::util::is_ci_environment() {
        eprintln!(
            "Skipping e2e_byoyomi_oob_finalize_logs in CI environment (performance-dependent test)"
        );
        return;
    }
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

    let mut finalize_events: Vec<String> = Vec::new();
    let mut time_budget_line: Option<String> = None;
    let mut time_caps_line: Option<String> = None;

    let bestmove_seen = p.wait_for(6000, |line| {
        if line.contains("time_budget") {
            time_budget_line = Some(line.to_string());
        }
        if line.contains("time_caps") {
            time_caps_line = Some(line.to_string());
        }
        if line.contains("finalize_event ") {
            finalize_events.push(line.to_string());
        }
        line.starts_with("bestmove ")
    });

    assert!(bestmove_seen, "bestmove was not emitted after OOB finalize");
    assert!(!finalize_events.is_empty(), "finalize_event log was not observed");
    let saw_oob_event = finalize_events.iter().any(|line| line.contains("label=oob_"));
    let saw_joined_event = finalize_events.iter().any(|line| line.contains("mode=joined"));
    assert!(
        saw_oob_event || saw_joined_event,
        "expected OOB or joined finalize event, logs={:?}",
        finalize_events
    );

    fn parse_soft_hard(line: &str) -> Option<(u64, u64)> {
        let mut soft = None;
        let mut hard = None;
        for part in line.split_whitespace() {
            if let Some(rest) = part.strip_prefix("soft_ms=") {
                soft = rest.trim_end_matches(',').parse::<u64>().ok();
            }
            if let Some(rest) = part.strip_prefix("hard_ms=") {
                hard = rest.trim_end_matches(',').parse::<u64>().ok();
            }
        }
        match (soft, hard) {
            (Some(s), Some(h)) => Some((s, h)),
            _ => None,
        }
    }

    let (budget_soft, budget_hard) =
        parse_soft_hard(time_budget_line.as_deref().expect("time_budget log missing"))
            .expect("failed parsing time_budget");
    if let Some(line) = time_caps_line.as_deref() {
        let (caps_soft, caps_hard) = parse_soft_hard(line).expect("failed parsing time_caps");
        assert_eq!(caps_hard, budget_hard, "time_caps hard should match time_budget hard");
        assert!(
            caps_soft >= budget_soft,
            "time_caps soft should not undershoot time_budget (caps_soft={} budget_soft={})",
            caps_soft,
            budget_soft
        );
    }
}

#[test]
#[ignore]
fn e2e_stop_flag_reset_after_oob_finalize() {
    let mut p = UsiProc::spawn();
    p.write_line("usi");
    assert!(p.wait_for_contains("usiok", 2000), "usiok timeout");

    p.write_line("setoption name Threads value 2");
    p.write_line("setoption name MinThinkMs value 2000");
    p.write_line("setoption name ByoyomiDeadlineLeadMs value 0");
    p.write_line("setoption name ByoyomiSafetyMs value 0");
    p.write_line("setoption name OverheadMs value 0");
    p.write_line("setoption name ByoyomiOverheadMs value 0");
    p.write_line("setoption name StopWaitMs value 0");
    p.write_line("setoption name Ponder value false");

    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000), "readyok timeout after option updates");

    p.write_line("usinewgame");
    p.write_line("position startpos");

    let mut first_addr: Option<String> = None;
    let mut first_finalize_events = 0u32;
    p.write_line("go btime 0 wtime 0 byoyomi 2000");
    let first_bestmove = p.wait_for(6000, |line| {
        if line.contains("stop_flag_create addr=") && first_addr.is_none() {
            if let Some(addr) = line.split("stop_flag_create addr=").nth(1) {
                if let Some(head) = addr.split_whitespace().next() {
                    first_addr = Some(head.to_string());
                }
            }
        }
        if line.contains("finalize_event ") {
            first_finalize_events = first_finalize_events.saturating_add(1);
        }
        line.starts_with("bestmove ")
    });
    assert!(first_bestmove, "first bestmove not observed");
    let first_addr = first_addr.expect("first stop_flag address missing");
    assert!(first_finalize_events > 0, "first go did not trigger finalize event");

    p.write_line("isready");
    assert!(p.wait_for_contains("readyok", 2000), "readyok timeout after first search");

    p.write_line("position startpos");

    let mut second_addr: Option<String> = None;
    let mut second_finalize_events = 0u32;
    p.write_line("go btime 0 wtime 0 byoyomi 2000");
    let second_bestmove = p.wait_for(6000, |line| {
        if line.contains("stop_flag_create addr=") && second_addr.is_none() {
            if let Some(addr) = line.split("stop_flag_create addr=").nth(1) {
                if let Some(head) = addr.split_whitespace().next() {
                    second_addr = Some(head.to_string());
                }
            }
        }
        if line.contains("finalize_event ") {
            second_finalize_events = second_finalize_events.saturating_add(1);
        }
        line.starts_with("bestmove ")
    });
    assert!(second_bestmove, "second bestmove not observed");
    let second_addr = second_addr.expect("second stop_flag address missing");
    assert!(second_finalize_events > 0, "second go did not trigger finalize event");

    assert_ne!(first_addr, second_addr, "stop_flag address must change between OOB finalizes");
}

#[test]
#[ignore]
fn e2e_ponder_stop_then_go_fast() {
    if engine_core::util::is_ci_environment() {
        eprintln!(
            "Skipping e2e_ponder_stop_then_go_fast in CI environment (performance-dependent test)"
        );
        return;
    }
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
        assert!(
            p.wait_for_contains("finalize_event label=stop_finalize", 2000),
            "stop_finalize event missing"
        );
        assert!(p.wait_for_contains("bestmove ", 2000));

        p.write_line("position startpos moves 7g7f 3c3d 2g2f");
        p.write_line("go btime 0 wtime 0 byoyomi 2000");
        assert!(
            p.wait_for_contains("session_publish stop_ctrl sid=", 1000),
            "session_publish log missing"
        );
        assert!(p.wait_for_contains("bestmove ", 4000));
    }
}
