use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workflow::{self, Error, WorkflowResult};

fn compile_to(root: &Path, out: &Path) -> WorkflowResult {
    // Keep rm's original operand handling and root/dot-directory protections.
    workflow::checked(Command::new("rm").current_dir(root).arg("-rf").arg(out))?;
    workflow::checked(
        Command::new("npx")
            .current_dir(root)
            .args(["tsc", "-p", "tsconfig.frontend-node-test.json", "--outDir"])
            .arg(out),
    )
}

pub(super) fn tests(root: &Path, args: &[&str]) -> WorkflowResult {
    let tmp_base = super::env_path("TMPDIR", "/tmp");
    let build = workflow::TempDir::new("restream-frontend-node-test-js")?;
    let result = compile_to(root, build.path()).and_then(|()| {
        let mut command = node_command(args.first() == Some(&"--coverage"));
        command
            .current_dir(root)
            .env("FRONTEND_MODULES_DIR", build.path())
            .env("TMPDIR", &tmp_base)
            .args(test_files(root)?);
        workflow::checked(&mut command)
    });
    if env::var_os("FRONTEND_NODE_TEST_KEEP_BUILD_DIR") == Some(OsString::from("1")) {
        std::mem::forget(build);
    }
    result
}

fn node_command(coverage: bool) -> Command {
    let mut command = Command::new("node");
    command.args(["--experimental-default-type=module", "--enable-source-maps"]);
    if coverage {
        command.arg("--experimental-test-coverage");
    }
    command.arg("--test");
    if coverage {
        command.arg("--test-coverage-exclude=test/**");
    }
    command
}

fn test_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root.join("test/frontend"))? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().ends_with(".test.mjs")
            && !entry.file_name().to_string_lossy().starts_with('.')
        {
            files.push(Path::new("test/frontend").join(entry.file_name()));
        }
    }
    files.sort();
    if files.is_empty() {
        // Bash retains an unmatched glob, letting Node report the missing test.
        files.push(PathBuf::from("test/frontend/*.test.mjs"));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = env::temp_dir().join(format!(
            "restream-frontend-node-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn node_test_selection_includes_only_unit_test_glob_in_order() {
        let root = scratch();
        fs::create_dir_all(root.join("test/frontend")).unwrap();
        for name in ["z.test.mjs", "a.spec.ts", "a.test.mjs", ".hidden.test.mjs"] {
            fs::write(root.join("test/frontend").join(name), "").unwrap();
        }
        assert_eq!(
            test_files(&root).unwrap(),
            [
                PathBuf::from("test/frontend/a.test.mjs"),
                PathBuf::from("test/frontend/z.test.mjs"),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
