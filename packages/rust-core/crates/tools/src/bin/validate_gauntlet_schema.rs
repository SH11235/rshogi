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
    let schema: Value = serde_json::from_reader(schema_reader)
        .with_context(|| format!("failed to parse schema JSON: {}", opt.schema.display()))?;
    // JSONSchema keeps references to the provided Value; leaking a boxed schema keeps it alive
    // for the short lifetime of this CLI process.
    let schema_ref: &'static Value = Box::leak(Box::new(schema));

    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft202012)
        .compile(schema_ref)
        .with_context(|| format!("failed to compile schema: {}", opt.schema.display()))?;

    let input_reader = BufReader::new(
        File::open(&opt.input)
            .with_context(|| format!("failed to open input file: {}", opt.input.display()))?,
    );
    let data: Value = serde_json::from_reader(input_reader)
        .with_context(|| format!("failed to parse input JSON: {}", opt.input.display()))?;

    if let Err(errors) = compiled.validate(&data) {
        let collected: Vec<_> = errors.collect();
        eprintln!("schema validation failed:");
        for err in &collected {
            eprintln!(
                "  -> {} (instance path: {}, schema path: {})",
                err, err.instance_path, err.schema_path
            );
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
