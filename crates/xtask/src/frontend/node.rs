use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workflow::{self, Error, WorkflowResult};

pub(super) fn compile(root: &Path, args: &[&str]) -> WorkflowResult {
    let out = args
        .first()
        .ok_or_else(|| Error::exit(1, "compile-for-node: missing output directory"))?;
    compile_to(root, Path::new(out))
}

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

// Match rm -rf: unlink a symlink itself, not the directory it points to.
fn remove_output(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn tests(root: &Path, args: &[&str]) -> WorkflowResult {
    let tmp_base = super::env_path("TMPDIR", "/tmp");
    let build = TempBuild::create(&tmp_base)?;
    compile_to(root, &build.path)?;
    let mut command = node_command(args.first() == Some(&"--coverage"));
    command
        .current_dir(root)
        .env("FRONTEND_MODULES_DIR", &build.path)
        .env("TMPDIR", &tmp_base)
        .args(test_files(root)?);
    workflow::checked(&mut command)
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

struct TempBuild {
    path: PathBuf,
    keep: bool,
}

impl TempBuild {
    fn create(base: &Path) -> Result<Self, Error> {
        let output = workflow::output(
            Command::new("mktemp")
                .arg("-d")
                .arg(base.join("restream-frontend-node-test-js.XXXXXX")),
        )?;
        let mut bytes = output.stdout;
        while bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(OsString::from_vec(bytes))
        };
        #[cfg(not(unix))]
        let path = PathBuf::from(String::from_utf8(bytes).map_err(|error| error.to_string())?);
        Ok(Self {
            path,
            keep: env::var_os("FRONTEND_NODE_TEST_KEEP_BUILD_DIR") == Some(OsString::from("1")),
        })
    }
}

impl Drop for TempBuild {
    fn drop(&mut self) {
        if !self.keep {
            let _ = remove_output(&self.path);
        }
    }
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
    fn temp_build_removes_partial_compiler_output_on_error() {
        let path = scratch();
        let result: io::Result<()> = (|| {
            let _build = TempBuild {
                path: path.clone(),
                keep: false,
            };
            fs::write(path.join("partial.js"), "partial")?;
            Err(io::Error::other(
                "compiler failed after writing partial output",
            ))
        })();
        assert!(result.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn requested_build_preservation_keeps_output() {
        let path = scratch();
        {
            let _build = TempBuild {
                path: path.clone(),
                keep: true,
            };
        }
        assert!(path.is_dir());
        fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn removing_compiler_output_unlinks_symlink_without_removing_target() {
        let parent = scratch();
        let target = parent.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.js"), "keep").unwrap();
        let link = parent.join("output");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        remove_output(&link).unwrap();
        assert!(target.join("keep.js").is_file());
        assert!(fs::symlink_metadata(link).is_err());
        fs::remove_dir_all(parent).unwrap();
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
