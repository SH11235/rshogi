use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = BTreeMap::new();
    let mut key = String::new();
    let mut newgame = false;
    let mut ready = false;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if let Some(path) = config.get("log") {
            let mut log = OpenOptions::new().create(true).append(true).open(path)?;
            log.write_all(format!("[{}, {:?}]\n", std::process::id(), line).as_bytes())?;
        }
        if line == "usi" {
            for name in ["FixtureFile", "EvalFile", "Threads", "USI_Hash", "MultiPV"] {
                println!("option name {name} type string default empty");
            }
            println!("usiok");
        } else if let Some(path) = line.strip_prefix("setoption name FixtureFile value ") {
            config = fs::read_to_string(path)?
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
        } else if line == "usinewgame" {
            newgame = true;
            ready = false;
        } else if line == "isready" {
            ready = newgame;
            println!("readyok");
        } else if let Some(sfen) = line.strip_prefix("position sfen ") {
            key = sfen.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
        } else if line.starts_with("go ") {
            if !newgame || !ready {
                std::process::exit(9);
            }
            newgame = false;
            if config.get("crash_always").is_some_and(|s| s == "true")
                || config.get("crash_key") == Some(&key)
            {
                std::process::exit(8);
            }
            if let Some(path) = config.get("crash_once") {
                match OpenOptions::new().create_new(true).write(true).open(path) {
                    Ok(file) => {
                        drop(file);
                        std::process::exit(7);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let resign = config.get("unknown_resign").is_some_and(|s| s == "true");
            let rows = match config.get(&key) {
                Some(rows) => rows.as_str(),
                None if resign => "",
                None => return Err("局面がありません".into()),
            };
            let rows: Vec<_> = rows.split('|').filter(|r| !r.is_empty()).collect();
            let delay: f64 = config.get("delay").map(|v| v.parse()).transpose()?.unwrap_or(0.0);
            let started = Instant::now();
            while started.elapsed().as_secs_f64() < delay {
                if config.get("stagnant").is_some_and(|s| s == "true") && !rows.is_empty() {
                    let mv = rows[0].split_whitespace().next().ok_or("手がありません")?;
                    println!("info depth 1 nodes 1 score cp 0 pv {mv}");
                    println!("info string alive");
                    io::stdout().flush()?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let noisy = config.get("noisy").is_some_and(|s| s == "true");
            if noisy && let Some(row) = rows.first() {
                let mv = row.split_whitespace().next().ok_or("手がありません")?;
                println!("info depth 2 multipv 1 score cp 3 pv {mv}");
            }
            for (index, row) in rows.iter().enumerate() {
                let fields: Vec<_> = row.split_whitespace().collect();
                println!(
                    "info depth {} nodes 101 multipv {} score cp {} pv {}",
                    fields[2],
                    index + 1,
                    fields[1],
                    fields[0]
                );
            }
            if noisy && let Some(row) = rows.first() {
                let mv = row.split_whitespace().next().ok_or("手がありません")?;
                println!("info depth 99 nodes 150 multipv 1 score cp 12345 lowerbound pv {mv}");
            }
            let bestmove =
                config.get("bestmove").map(String::as_str).unwrap_or_else(|| {
                    rows.first().and_then(|r| r.split_whitespace().next()).unwrap_or("resign")
                });
            println!("bestmove {bestmove}");
        } else if line == "quit" {
            break;
        }
        io::stdout().flush()?;
    }
    Ok(())
}
