use std::io::{self, Write};

/// USIプロトコルに沿って標準出力へ行を出力するヘルパ。
pub fn usi_println(s: &str) {
    println!("{s}");
    io::stdout().flush().unwrap();
}

/// `info string ...` の出力ユーティリティ。
pub fn info_string<S: AsRef<str>>(s: S) {
    usi_println(&format!("info string {}", s.as_ref()));
}
