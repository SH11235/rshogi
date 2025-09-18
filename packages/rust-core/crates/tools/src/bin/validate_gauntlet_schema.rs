use std::{fs::File, io::BufReader, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::Parser;
use jsonschema::{Draft, JSONSchema};
use serde_json::Value;

/// Validate gauntlet out.json against the repository schema
#[derive(Parser, Debug)]
#[command(name = "validate_gauntlet_schema", version, about)]
struct Opt {
    /// Path to gauntlet_out.schema.json
    #[arg(long)]
    schema: PathBuf,
    /// Path to gauntlet out.json
    #[arg(long)]
    input: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let opt = Opt::parse();

    let schema_reader = BufReader::new(
        File::open(&opt.schema)
            .with_context(|| format!("failed to open schema file: {}", opt.schema.display()))?,
    );
    let leaked_schema: &'static Value = {
        let schema: Value = serde_json::from_reader(schema_reader)
            .with_context(|| format!("failed to parse schema JSON: {}", opt.schema.display()))?;
        Box::leak(Box::new(schema))
    };

    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft202012)
        .compile(leaked_schema)
        .with_context(|| format!("failed to compile schema: {}", opt.schema.display()))?;

    let input_reader = BufReader::new(
        File::open(&opt.input)
            .with_context(|| format!("failed to open input file: {}", opt.input.display()))?,
    );
    let data: Value = serde_json::from_reader(input_reader)
        .with_context(|| format!("failed to parse input JSON: {}", opt.input.display()))?;

    if let Err(errors) = compiled.validate(&data) {
        eprintln!("schema validation failed:");
        for err in errors {
            eprintln!("  -> {}", err);
        }
        anyhow::bail!("validation errors detected")
    }

    println!(
        "schema validation ok (schema={}, input={})",
        opt.schema.display(),
        opt.input.display()
    );
    Ok(())
}
