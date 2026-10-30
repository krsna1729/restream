use super::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn a_group_runs_every_step_and_fails_if_any_failed() {
    static RAN: AtomicUsize = AtomicUsize::new(0);
    const FAILS: Step = Step::builtin("fails", || Err("broken".into()));
    const COUNTS: Step = Step::builtin("counts", || {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    const EXITS: Step = Step::command(
        "exits",
        Cmd {
            program: "sh",
            args: &["-c", "exit 3"],
            env: &[],
        },
    );

    let failed = run_group(&[&FAILS, &COUNTS, &EXITS, &COUNTS]).unwrap_err();

    assert_eq!(RAN.load(Ordering::SeqCst), 2);
    assert_eq!(failed.to_string(), "failed: fails, exits");
    assert!(run_group(&[&COUNTS, &COUNTS]).is_ok());
}

#[test]
fn a_command_step_sees_its_environment() {
    const ENV: Step = Step::command(
        "env",
        Cmd {
            program: "sh",
            args: &[
                "-c",
                r#"test "$XTASK_CHECK" = set && test -z "$XTASK_EMPTY""#,
            ],
            env: &[("XTASK_CHECK", "set"), ("XTASK_EMPTY", "")],
        },
    );
    assert!(run_step(&ENV).is_ok());
}

#[test]
fn a_missing_tool_fails_the_step_with_its_install_hint() {
    const MISSING: Step = Step::builtin("missing", || unreachable!("tool was checked first"))
        .needs(Tool {
            probe: &["xtask-no-such-tool", "--version"],
            hint: "cargo install xtask-no-such-tool --locked",
        });

    let error = run_step(&MISSING).unwrap_err().to_string();

    assert!(
        error.contains("cargo install xtask-no-such-tool --locked"),
        "{error}"
    );
}

#[test]
fn every_name_runs_one_check_and_other_arguments_are_not_checks() {
    let groups: HashSet<&str> = GROUPS.iter().map(|group| group.name).collect();
    assert_eq!(groups.len(), GROUPS.len(), "two groups share a name");
    for step in steps() {
        assert!(!groups.contains(step.name), "{} names a group", step.name);
        assert!(
            steps()
                .filter(|other| other.name == step.name)
                .all(|other| std::ptr::eq(other, step)),
            "two steps are named {}",
            step.name
        );
    }
    assert!(run(&["nope"]).is_none());
    assert!(run(&["docs-audit", "extra"]).is_none());
    assert!(run(&[]).is_none());
}

const KANI_LOG: &str = "\
Checking harness a...
 ** 2 of 2 cover properties satisfied
Checking harness b...
 ** 1 of 1 cover properties satisfied
Complete - 7 successfully verified harnesses, 0 failures, 7 total.
";

#[test]
fn kani_passes_only_all_proofs_with_every_cover_reached() {
    assert_eq!(kani_verdict(KANI_LOG, 7), Ok(()));

    assert!(kani_verdict(KANI_LOG, 8).is_err(), "a proof vanished");
    let failure = KANI_LOG.replace(
        "7 successfully verified harnesses, 0 failures",
        "6 successfully verified harnesses, 1 failures",
    );
    assert!(kani_verdict(&failure, 7).is_err());
    let unreached = KANI_LOG.replace("2 of 2 cover", "1 of 2 cover");
    assert!(kani_verdict(&unreached, 7).is_err());
    let no_covers: String = KANI_LOG
        .lines()
        .filter(|line| !line.contains("cover"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(kani_verdict(&no_covers, 7).is_err());
}
