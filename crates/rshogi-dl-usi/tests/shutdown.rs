#![cfg(feature = "onnx")]

use std::io::{self, Write};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn run(input: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rshogi-dl-usi"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        child.stdin.take().expect("piped stdin").write_all(input)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "USI shutdown timed out"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    child.wait_with_output()
}

#[test]
fn quit_and_eof_flush_handshake_and_exit_successfully() -> io::Result<()> {
    for input in [b"usi\nquit\n".as_slice(), b"usi\n".as_slice()] {
        let output = run(input)?;
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stdout.ends_with(b"usiok\n"));
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    }
    Ok(())
}

#[test]
fn invalid_stdin_exits_with_error() -> io::Result<()> {
    let output = run(&[0xff, b'\n'])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
    Ok(())
}
