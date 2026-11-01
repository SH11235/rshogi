use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = BTreeMap::new();
    let mut key = String::new();
    let mut newgame = false;
    let mut ready = false;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    while let Ok(line) = rx.recv() {
        let line = line?;
        if let Some(path) = config.get("log") {
            let mut log = OpenOptions::new().create(true).append(true).open(path)?;
            log.write_all(format!("[{}, {:?}]\n", std::process::id(), line).as_bytes())?;
        }
        if line == "usi" {
            for name in [
                "FixtureFile",
                "EvalFile",
                "LS_PROGRESS_COEFF",
                "Threads",
                "USI_Hash",
                "MultiPV",
            ] {
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
            let phase = if newgame { "newgame" } else { "initial" };
            if config.get("ready_phase").is_some_and(|s| s == phase) {
                fs::write(config.get("ready_marker").ok_or("missing ready_marker")?, "")?;
                let release = config.get("ready_release").ok_or("missing ready_release")?;
                let started = Instant::now();
                while !std::path::Path::new(release).try_exists()? {
                    if started.elapsed() > Duration::from_secs(30) {
                        return Err("isready barrier timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            ready = newgame;
            println!("readyok");
        } else if let Some(sfen) = line.strip_prefix("position sfen ") {
            key = sfen.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
        } else if line.starts_with("go ") {
            if !newgame || !ready {
                std::process::exit(9);
            }
            newgame = false;
            let mut crash_key = config.get("crash_key") == Some(&key);
            if crash_key && let Some(path) = config.get("crash_count_file") {
                let count: usize = match fs::read_to_string(path) {
                    Ok(s) => s.parse()?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
                    Err(e) => return Err(e.into()),
                };
                let limit: usize = config.get("crash_limit").ok_or("crash_limit")?.parse()?;
                crash_key = count < limit;
                if crash_key {
                    fs::write(path, (count + 1).to_string())?;
                }
            }
            if config.get("crash_always").is_some_and(|s| s == "true") || crash_key {
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
            let delay: f64 = config
                .get(&format!("delay:{key}"))
                .or_else(|| config.get("delay"))
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(0.0);
            let started = Instant::now();
            let mut stopped = false;
            let mut nodes = 0;
            let flood = config.get("flood_key") == Some(&key);
            while started.elapsed().as_secs_f64() < delay {
                if flood {
                    let mut stdout = io::stdout().lock();
                    for _ in 0..4096 {
                        writeln!(stdout, "info string alive")?;
                    }
                    stdout.flush()?;
                }
                match rx.recv_timeout(if flood { Duration::ZERO } else { Duration::from_millis(50) }) {
                    Ok(line) => {
                        let line = line?;
                        if let Some(path) = config.get("log") {
                            let mut log =
                                OpenOptions::new().create(true).append(true).open(path)?;
                            writeln!(log, "[{}, {:?}]", std::process::id(), line)?;
                        }
                        if line == "quit" {
                            return Ok(());
                        }
                        if line == "stop" {
                            stopped = true;
                            match config.get("stop_mode").map(String::as_str).unwrap_or("respond") {
                                "eof" => return Ok(()),
                                "ignore" | "info" => {}
                                _ => {
                                    if let Some(delay) = config.get("stop_delay_ms") {
                                        std::thread::sleep(Duration::from_millis(delay.parse()?));
                                    }
                                    println!("bestmove resign");
                                    io::stdout().flush()?;
                                    break;
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                if config.get("growing").is_some_and(|s| s == "true")
                    || (stopped && config.get("stop_mode").is_some_and(|s| s == "info"))
                {
                    nodes += 1;
                    println!("info nodes {nodes} depth 1");
                    io::stdout().flush()?;
                }
                if config.get("stagnant").is_some_and(|s| s == "true") && !rows.is_empty() {
                    let mv = rows[0].split_whitespace().next().ok_or("手がありません")?;
                    println!("info depth 1 nodes 1 score cp 0 pv {mv}");
                    println!("info string alive");
                    io::stdout().flush()?;
                }
            }
            if stopped {
                continue;
            }
            let noisy = config.get("noisy").is_some_and(|s| s == "true");
            if noisy && let Some(row) = rows.first() {
                let mv = row.split_whitespace().next().ok_or("手がありません")?;
                println!("info depth 2 multipv 1 score cp 3 pv {mv}");
            }
            let node_field = if config.get("no_nodes").is_some_and(|s| s == "true") {
                String::new()
            } else {
                format!("nodes {}", config.get("final_nodes").map(String::as_str).unwrap_or("101"))
            };
            for (index, row) in rows.iter().enumerate() {
                let fields: Vec<_> = row.split_whitespace().collect();
                println!(
                    "info depth {} {node_field} multipv {} score cp {} pv {}",
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
            let bestmove = config.get("bestmove").map(String::as_str).unwrap_or_else(|| {
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
