use crate::workflow::{Error, WorkflowResult};
use std::path::Path;
use std::process::{Command, Stdio};

fn selected(
    dir: &Path,
    command: &[&str],
    envs: &[(&str, &str)],
    filter: &str,
) -> Result<usize, Error> {
    let output = crate::workflow::output(
        Command::new(command[0])
            .current_dir(dir)
            .args(&command[1..])
            .envs(envs.iter().copied())
            .args(["--", filter, "--list", "--format", "terse"])
            .stderr(Stdio::piped()),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.ends_with(": test"))
        .count())
}

/// `require-test-filters`: the original ran the command from the caller's
/// directory.
pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    require(&crate::invocation_dir(), args, &[])
}

/// Lists the tests `<cargo test ...> -- <filter>...` selects, one filter at
/// a time, without running them; a filter that selects no test fails.
pub(crate) fn require(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> WorkflowResult {
    let Some(separator) = args.iter().position(|arg| *arg == "--") else {
        return Err(Error::usage(
            "usage: test-filters <cargo test ...> -- <filter>...",
        ));
    };
    if separator == 0 {
        return Err(Error::usage("no test command given"));
    }
    if separator + 1 == args.len() {
        return Err(Error::usage("no filters given"));
    }
    let mut failed = false;
    for filter in &args[separator + 1..] {
        match selected(dir, &args[..separator], envs, filter) {
            Ok(0) => {
                eprintln!("filter '{filter}' selects no test");
                failed = true;
            }
            Ok(count) => println!("filter '{filter}': {count} tests"),
            Err(error) => {
                eprintln!("{error}");
                failed = true;
            }
        }
    }
    if failed {
        Err(Error::exit(1, "test filter guard failed"))
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_separator_and_filters_are_rejected() {
        assert!(run(&["cargo", "test"]).is_err());
        assert!(run(&["cargo", "test", "--"]).is_err());
        assert!(run(&["--", "filter"]).is_err());
    }
    #[test]
    fn actual_command_failure_is_not_a_selection() {
        assert!(selected(Path::new("."), &["/usr/bin/false"], &[], "anything").is_err());
    }
    #[test]
    fn actual_command_counts_only_terse_tests() {
        // printf ignores surplus arguments after its format, exercising the actual
        // subprocess boundary without invoking a compiler or interpreting shell.
        assert_eq!(
            selected(
                Path::new("."),
                &[
                    "/usr/bin/printf",
                    "one: test\ntwo: benchmark\nthree: test\n"
                ],
                &[],
                "a filter with spaces"
            )
            .unwrap(),
            2
        );
    }
}
