use std::io::{self, Write};

pub(super) fn finish(result: anyhow::Result<()>) -> ! {
    let mut code = match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:?}");
            1
        }
    };
    let stdout = io::stdout().flush();
    let stderr = io::stderr().flush();
    if stdout.is_err() || stderr.is_err() {
        code = 1;
    }
    #[cfg(all(unix, feature = "onnx"))]
    // SAFETY: _exit は追加のメモリ参照を行わず、ORT の global teardown を避ける。
    // run_command で session を解放し、stdout/stderr を flush した後だけ呼ぶ。
    unsafe {
        libc::_exit(code);
    }
    #[cfg(not(all(unix, feature = "onnx")))]
    std::process::exit(code)
}
