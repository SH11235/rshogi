use std::fs::{self, File};
use std::io::Write;
use std::sync::Mutex;

/// 構造化JSONログを扱うヘルパ。
pub struct StructuredLogger {
    pub to_stdout: bool,
    file: Option<Mutex<std::io::BufWriter<File>>>,
}

impl StructuredLogger {
    pub fn new(path: &str) -> std::io::Result<Self> {
        if path == "-" {
            Ok(Self {
                to_stdout: true,
                file: None,
            })
        } else {
            if let Some(parent) = std::path::Path::new(path).parent() {
                fs::create_dir_all(parent)?;
            }
            let f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            let bw = std::io::BufWriter::with_capacity(1 << 20, f);
            Ok(Self {
                to_stdout: false,
                file: Some(Mutex::new(bw)),
            })
        }
    }

    pub fn write_json(&self, v: &serde_json::Value) {
        if self.to_stdout {
            println!("{}", v);
        } else if let Some(ref file) = self.file {
            if let Ok(mut w) = file.lock() {
                let _ = writeln!(w, "{}", v);
            }
        }
    }

    /// 明示的に内部バッファを flush する。stdout モードの場合は何もしない。
    #[cfg(test)]
    pub fn flush(&self) -> std::io::Result<()> {
        if let Some(ref file) = self.file {
            let mut w = file.lock().unwrap();
            std::io::Write::flush(&mut *w)
        } else {
            Ok(())
        }
    }
}

/// ゼロ重みのバッチをエポック毎にダンプするデバッグヘルパ。
#[inline]
pub fn print_zero_weight_debug(epoch: usize, count: usize, structured: &Option<StructuredLogger>) {
    if count == 0 {
        return;
    }
    let human_to_stderr = structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false);
    if human_to_stderr {
        eprintln!("[debug] epoch {} had {} zero-weight batches", epoch + 1, count);
    } else {
        println!("[debug] epoch {} had {} zero-weight batches", epoch + 1, count);
    }
}
