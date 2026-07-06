//! Manifest-backed integration harness planner.
//!
//! This binary is intentionally additive: it validates and resolves the
//! `test/harness` DSL without disturbing the existing `test_harness` runner.
//!
//! Usage:
//!   cargo run --bin test_harness_dsl -- self-check
//!   cargo run --bin test_harness_dsl -- list-modes
//!   cargo run --bin test_harness_dsl -- resolve mixed.live.srt.h265.a2.bf2
//!   cargo run --bin test_harness_dsl -- plan mixed.matrix
//!
//! The planner emits JSON and resolves canonical suite/runner/scenario modes.

#[path = "test_harness/catalog.rs"]
mod catalog;
use catalog::*;

use serde_json::{Value, json};
use std::env;
use std::path::PathBuf;
use std::process;

#[derive(Debug, Clone)]
struct Cli {
    root: PathBuf,
    json: bool,
    command: String,
    args: Vec<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let cli = parse_cli()?;

    match cli.command.as_str() {
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "self-check" => {
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "passed": true,
                    "root": catalog.root,
                    "summary": catalog.summary()
                }))
                .unwrap()
            );
            Ok(())
        }
        "list-modes" => {
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            let modes = catalog.list_modes()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&modes).unwrap());
            } else {
                print_mode_list(&modes);
            }
            Ok(())
        }
        "resolve" => {
            let mode = required_arg(&cli, "resolve <mode>")?;
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            let resolved = catalog.resolve_mode(mode)?;
            println!("{}", serde_json::to_string_pretty(&resolved).unwrap());
            Ok(())
        }
        "plan" => {
            let mode = required_arg(&cli, "plan <mode>")?;
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            let plan = catalog.plan_mode(mode)?;
            println!("{}", serde_json::to_string_pretty(&plan).unwrap());
            Ok(())
        }
        other => Err(format!(
            "unknown command {other:?}; run `test_harness_dsl help`"
        )),
    }
}

fn parse_cli() -> AppResult<Cli> {
    let raw: Vec<String> = env::args().skip(1).collect();
    let mut root = env::var_os("HARNESS_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("test/harness"));
    let mut json = false;
    let mut positionals = Vec::new();

    let mut i = 0usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--root" => {
                i += 1;
                let value = raw
                    .get(i)
                    .ok_or("--root requires a path argument".to_string())?;
                root = PathBuf::from(value);
            }
            "--json" => json = true,
            "-h" | "--help" => positionals.push("help".to_string()),
            item if item.starts_with("--root=") => {
                root = PathBuf::from(item.trim_start_matches("--root="));
            }
            item => positionals.push(item.to_string()),
        }
        i += 1;
    }

    let command = positionals
        .first()
        .cloned()
        .unwrap_or_else(|| "help".to_string());
    let args = positionals.into_iter().skip(1).collect();

    Ok(Cli {
        root,
        json,
        command,
        args,
    })
}

fn required_arg<'a>(cli: &'a Cli, usage: &str) -> AppResult<&'a str> {
    cli.args
        .first()
        .map(String::as_str)
        .ok_or_else(|| format!("usage: test_harness_dsl {usage}"))
}

fn print_usage() {
    println!(
        r#"test_harness_dsl

Usage:
  test_harness_dsl [--root test/harness] self-check
  test_harness_dsl [--root test/harness] list-modes [--json]
  test_harness_dsl [--root test/harness] resolve <mode>
  test_harness_dsl [--root test/harness] plan <mode>

Environment:
  HARNESS_CATALOG_DIR  Defaults the manifest root, usually test/harness.

Notes:
  - `plan` is manifest execution planning, not FFmpeg/API execution.
  - exact mixed scenario ids are accepted as dynamic modes.
"#
    );
}

fn print_mode_list(value: &Value) {
    let Some(groups) = value.get("groups").and_then(Value::as_array) else {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
        return;
    };

    for group in groups {
        let group_name = group
            .get("group")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let kind = group
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("{group_name} ({kind})");
        if let Some(modes) = group.get("modes").and_then(Value::as_array) {
            for mode in modes {
                let name = mode
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                let mut suffix = String::new();
                if let Some(suite_ref) = mode.get("suiteRef").and_then(Value::as_str) {
                    suffix = format!(" -> {suite_ref}");
                }
                println!("  {name}{suffix}");
            }
        }
        println!();
    }
}
