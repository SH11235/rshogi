//! Windows の book_mine 単体テスト用 USI mock。rustc で一度だけビルドする。
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};

fn main() {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    let table = fs::read_to_string(dir.join("table.tsv")).unwrap();
    let config = fs::read_to_string(dir.join("config.txt")).unwrap();
    let mut multipv = 1;
    let mut position = String::new();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line == "usi" {
            println!("id name mock");
            if config.starts_with("true") {
                println!("option name MultiPV type spin default 1 min 1 max 800");
            } else {
                println!("option name USI_Hash type spin default 256 min 1 max 1024");
            }
            println!("usiok");
        } else if line == "isready" {
            println!("readyok");
        } else if let Some(value) = line.strip_prefix("setoption name MultiPV value ") {
            multipv = value.parse().unwrap();
        } else if let Some(sfen) = line.strip_prefix("position sfen ") {
            position = sfen.to_owned();
        } else if line.starts_with("go") {
            let searchmove = line.split_once(" searchmoves ").map(|(_, mv)| mv);
            let mut log = OpenOptions::new().create(true).append(true).open(dir.join("log.txt")).unwrap();
            write!(log, "{multipv} {position}").unwrap();
            if let Some(mv) = searchmove { write!(log, " searchmoves {mv}").unwrap(); }
            writeln!(log).unwrap();
            let row = table.lines().find(|row| row.split('\t').next() == Some(&position));
            let Some(row) = row else {
                println!("bestmove resign");
                io::stdout().flush().unwrap();
                continue;
            };
            let fields: Vec<_> = row.split('\t').collect();
            let lines: Vec<_> = fields[2].split('|').collect();
            if let Some(mv) = searchmove.filter(|_| config.ends_with("true")) {
                if let Some(info) = lines.iter().find(|info| format!("{info} ").contains(&format!(" pv {mv} "))) {
                    emit(info, 1);
                    println!("bestmove {mv}");
                } else { println!("bestmove resign"); }
            } else {
                let limit = if searchmove.is_some() { 1 } else { multipv };
                for (idx, info) in lines.iter().take(limit).enumerate() { emit(info, idx + 1); }
                println!("bestmove {}", fields[1]);
            }
        } else if line == "quit" { break; }
        io::stdout().flush().unwrap();
    }
}

fn emit(info: &str, idx: usize) {
    if info.starts_with("info ") { println!("{info}"); }
    else if !info.is_empty() { println!("info depth 10 multipv {idx} {info}"); }
}
