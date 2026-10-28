//! Read-only catalog inspection commands for the integration harness.
//!
//! These commands replace the former standalone DSL helper. Keeping them under
//! `test_harness catalog ...` means release bundles ship one harness executable
//! while preserving the same manifest self-check, resolution, and
//! plan-inspection workflows.

use serde_json::{Value, json};
use std::path::PathBuf;

use crate::catalog::{AppResult, HarnessCatalog};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogCli {
    root: PathBuf,
    json: bool,
    command: String,
    args: Vec<String>,
}

pub(crate) fn run_catalog_cli(raw: &[String]) -> AppResult<()> {
    let cli = parse_catalog_cli(raw)?;

    match cli.command.as_str() {
        "help" | "-h" | "--help" => {
            print_catalog_usage();
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
            let mode = required_catalog_arg(&cli, "catalog resolve <mode>")?;
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            let resolved = catalog.resolve_mode(mode)?;
            println!("{}", serde_json::to_string_pretty(&resolved).unwrap());
            Ok(())
        }
        "plan" => {
            let mode = required_catalog_arg(&cli, "catalog plan <mode>")?;
            let catalog = HarnessCatalog::load(&cli.root)?;
            catalog.self_check()?;
            let plan = catalog.plan_mode(mode)?;
            println!("{}", serde_json::to_string_pretty(&plan).unwrap());
            Ok(())
        }
        other => Err(format!(
            "unknown catalog command {other:?}; run `test_harness catalog help`"
        )),
    }
}

fn parse_catalog_cli(raw: &[String]) -> AppResult<CatalogCli> {
    let mut root = std::env::var_os("HARNESS_CATALOG_DIR")
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

    Ok(CatalogCli {
        root,
        json,
        command,
        args,
    })
}

fn required_catalog_arg<'a>(cli: &'a CatalogCli, usage: &str) -> AppResult<&'a str> {
    cli.args
        .first()
        .map(String::as_str)
        .ok_or_else(|| format!("usage: test_harness {usage}"))
}

fn print_catalog_usage() {
    println!(
        r#"test_harness catalog

Usage:
  test_harness catalog [--root test/harness] self-check
  test_harness catalog [--root test/harness] list-modes [--json]
  test_harness catalog [--root test/harness] resolve <mode>
  test_harness catalog [--root test/harness] plan <mode>

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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn catalog_cli_defaults_to_help_without_live_runner_side_effects() {
        let cli = parse_catalog_cli(&[]).expect("parse default catalog cli");
        assert_eq!(cli.command, "help");
        assert_eq!(cli.root, PathBuf::from("test/harness"));
    }

    #[test]
    fn catalog_cli_accepts_root_json_and_mode_argument() {
        let cli = parse_catalog_cli(&strings(&[
            "--root",
            "custom/harness",
            "--json",
            "resolve",
            "mixed.matrix",
        ]))
        .expect("parse catalog cli");
        assert_eq!(cli.root, PathBuf::from("custom/harness"));
        assert!(cli.json);
        assert_eq!(cli.command, "resolve");
        assert_eq!(cli.args, vec!["mixed.matrix"]);
    }
}
