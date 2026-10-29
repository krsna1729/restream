use crate::workflow::{Error, WorkflowResult};
use std::process::{Command, Stdio};

fn selected(command: &[&str], filter: &str) -> Result<usize, Error> {
    // The original ran the command from the caller's directory.
    let output = crate::workflow::output(
        Command::new(command[0])
            .current_dir(crate::invocation_dir())
            .args(&command[1..])
            .args(["--", filter, "--list", "--format", "terse"])
            .stderr(Stdio::piped()),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.ends_with(": test"))
        .count())
}
pub(crate) fn run(args: &[&str]) -> WorkflowResult {
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
        match selected(&args[..separator], filter) {
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
        assert!(selected(&["/usr/bin/false"], "anything").is_err());
    }
    #[test]
    fn actual_command_counts_only_terse_tests() {
        // printf ignores surplus arguments after its format, exercising the actual
        // subprocess boundary without invoking a compiler or interpreting shell.
        assert_eq!(
            selected(
                &[
                    "/usr/bin/printf",
                    "one: test\ntwo: benchmark\nthree: test\n"
                ],
                "a filter with spaces"
            )
            .unwrap(),
            2
        );
    }
}
