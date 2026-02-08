use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;

const NOT_USED_MARKER: &str = "[[NOT USED]]";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "SPSA .params から shogitest 用 option 引数を生成する"
)]
struct Cli {
    /// path to .params file
    params_file: PathBuf,

    /// include parameters marked as [[NOT USED]]
    #[arg(long)]
    include_not_used: bool,

    /// output one option per line (default: single shell-ready line)
    #[arg(long)]
    one_per_line: bool,

    /// option prefix for shogitest (default: option.)
    #[arg(long, default_value = "option.")]
    prefix: String,
}

#[derive(Clone, Debug)]
struct Param {
    name: String,
    type_name: String,
    value: f64,
    not_used: bool,
}

impl Param {
    fn is_int(&self) -> bool {
        self.type_name.eq_ignore_ascii_case("int")
    }

    fn value_string(&self) -> String {
        if self.is_int() {
            return (self.value.round_ties_even() as i64).to_string();
        }
        let mut rendered = format!("{:.6}", self.value);
        while rendered.contains('.') && rendered.ends_with('0') {
            rendered.pop();
        }
        if rendered.ends_with('.') {
            rendered.pop();
        }
        rendered
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut params = load_params(&cli.params_file)?;
    if !cli.include_not_used {
        params.retain(|p| !p.not_used);
    }

    let rendered: Vec<String> = params
        .iter()
        .map(|p| format!("{}{}={}", cli.prefix, p.name, p.value_string()))
        .collect();

    if cli.one_per_line {
        for item in &rendered {
            println!("{item}");
        }
    } else {
        let quoted: Vec<String> = rendered.iter().map(|s| shell_quote(s)).collect();
        println!("{}", quoted.join(" "));
    }

    Ok(())
}

fn load_params(path: &PathBuf) -> Result<Vec<Param>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut params = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line_no = line_no + 1;
        let line = line.with_context(|| format!("failed to read line {line_no}"))?;
        if let Some(param) = parse_line(&line, line_no)? {
            params.push(param);
        }
    }
    Ok(params)
}

fn parse_line(line: &str, line_no: usize) -> Result<Option<Param>> {
    let mut raw = line.trim().to_owned();
    if raw.is_empty() || raw.starts_with('#') {
        return Ok(None);
    }

    let not_used = raw.contains(NOT_USED_MARKER);
    if not_used {
        raw = raw.replace(NOT_USED_MARKER, "");
    }
    if let Some((head, _)) = raw.split_once("//") {
        raw = head.to_owned();
    }

    let cols: Vec<&str> = raw.split(',').map(str::trim).collect();
    if cols.len() < 7 {
        bail!("line {line_no}: invalid params format: {line}");
    }

    let value = cols[2]
        .parse::<f64>()
        .with_context(|| format!("line {line_no}: invalid float value: {}", cols[2]))?;

    Ok(Some(Param {
        name: cols[0].to_owned(),
        type_name: cols[1].to_owned(),
        value,
        not_used,
    }))
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if is_shell_safe(value) {
        return value.to_owned();
    }
    let escaped = value.replace('\'', r"'\''");
    format!("'{escaped}'")
}

fn is_shell_safe(value: &str) -> bool {
    value.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || b == b'_'
            || b == b'@'
            || b == b'%'
            || b == b'+'
            || b == b'='
            || b == b':'
            || b == b','
            || b == b'.'
            || b == b'/'
            || b == b'-'
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_works() {
        let line = "SPSA_FOO,int,12.4,0,100,1,1 // comment [[NOT USED]]";
        let parsed = parse_line(line, 1).expect("parse failed").expect("none");
        assert_eq!(parsed.name, "SPSA_FOO");
        assert!(parsed.not_used);
        assert_eq!(parsed.value_string(), "12");
    }

    #[test]
    fn parse_line_skips_comment_and_empty() {
        assert!(parse_line("   ", 1).expect("parse failed").is_none());
        assert!(parse_line("# comment", 1).expect("parse failed").is_none());
    }

    #[test]
    fn shell_quote_behaviour() {
        assert_eq!(shell_quote("option.FOO=1"), "option.FOO=1");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn int_rounding_matches_python_style() {
        let p1 = Param {
            name: "A".to_owned(),
            type_name: "int".to_owned(),
            value: 12.5,
            not_used: false,
        };
        let p2 = Param {
            name: "B".to_owned(),
            type_name: "int".to_owned(),
            value: 13.5,
            not_used: false,
        };
        assert_eq!(p1.value_string(), "12");
        assert_eq!(p2.value_string(), "14");
    }
}
