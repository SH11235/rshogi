use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

fn main() -> io::Result<()> {
    if let Some(path) = std::env::var_os("BOOK_FINGERPRINT_EXE_LOG") {
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(log, "{}", std::env::current_exe()?.display())?;
    }
    let mut options = BTreeMap::<String, String>::new();
    for line in io::stdin().lock().lines() {
        let line = line?;
        match line.as_str() {
            "usi" => {
                for name in [
                    "EvalFile",
                    "EvalDir",
                    "BookFile",
                    "BookDir",
                    "BookExploreFile",
                    "SPSAParamsFile",
                    "CustomFile",
                    "USI_Hash",
                    "Threads",
                ] {
                    println!("option name {name} type string default <empty>");
                }
                println!("usiok");
            }
            "isready" => {
                if let Some(value) = options.get("EvalFile") {
                    let direct = Path::new(value);
                    let path = match options.get("EvalDir") {
                        Some(directory) if direct.is_relative() => {
                            Path::new(directory).join(direct)
                        }
                        _ => direct.to_owned(),
                    };
                    std::fs::read(path)?;
                }
                if let Some(value) =
                    options.get("BookFile").filter(|v| v.as_str() != "no_book" && !v.is_empty())
                {
                    let directory = options.get("BookDir").map(String::as_str).unwrap_or("book");
                    std::fs::read(Path::new(directory).join(value))?;
                }
                println!("readyok");
            }
            "quit" => break,
            _ if line.starts_with("setoption name ") => {
                if let Some((name, value)) = line[15..].split_once(" value ") {
                    options.insert(name.to_owned(), value.to_owned());
                }
            }
            _ if line.starts_with("go ") => {
                if let Some(path) = std::env::var_os("BOOK_FINGERPRINT_GO_LOG") {
                    let mut log =
                        std::fs::OpenOptions::new().create(true).append(true).open(path)?;
                    writeln!(log, "go")?;
                }
                println!("info depth 1 score cp 7 nodes 1 pv 7g7f");
                println!("bestmove 7g7f");
            }
            _ => {}
        }
        io::stdout().flush()?;
    }
    Ok(())
}
