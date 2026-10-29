use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::workflow::{self, Error, OwnedChild, WorkflowResult};

const MODES: [&str; 3] = [
    "mixed.live.rtmp.h264.a1.bf0",
    "mixed.live.srt.h265.a1.bf0",
    "mixed.live.rtmp.h264.a1.bf2",
];

const USAGE: &str = "usage: cargo xtask media-validation
Runs the bounded validation modes in order, appending their output to
.local/artifacts/latest/run.log. RESTREAM_REPO_ROOT and the harness environment apply.";

pub(crate) fn media_validation(args: &[&str]) -> WorkflowResult {
    if !super::no_arguments(args, USAGE)? {
        return Ok(());
    }
    let root = crate::repo_root();
    let artifacts = root.join(".local/artifacts/latest");
    fs::create_dir_all(&artifacts)?;
    let mut log = File::create(artifacts.join("run.log"))?;
    for mode in MODES {
        let marker = format!("== {mode} ==\n");
        io::stdout().write_all(marker.as_bytes())?;
        log.write_all(marker.as_bytes())?;
        // Capture preparation output as well as the actual mode, like the
        // original pipe to tee. Stderr remains the caller's stderr.
        let mut command = super::self_harness(&[mode])?;
        command.current_dir(&root);
        run_logged(&mut command, &log)?;
    }
    Ok(())
}

fn run_logged(command: &mut Command, log: &File) -> WorkflowResult {
    let mut owned = OwnedChild::spawn(command.stdout(Stdio::piped()))?;
    let output = owned
        .child()
        .stdout
        .take()
        .ok_or("harness stdout was not piped")?;
    let mut log = log.try_clone()?;
    let failed = AtomicBool::new(false);
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let result = copy_log(output, &mut io::stdout().lock(), &mut log);
            if result.is_err() {
                failed.store(true, Ordering::Release);
            }
            result
        });
        let status = loop {
            if failed.load(Ordering::Acquire) {
                owned.terminate();
                break Err(Error::from("cannot write media-validation output"));
            }
            if workflow::interrupted().is_some() {
                break owned.wait();
            }
            match owned.child().try_wait() {
                Ok(Some(status)) => {
                    owned.reaped();
                    break Ok(status);
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    owned.terminate();
                    break Err(error.into());
                }
            }
        };
        // Reap/terminate before joining: on cancellation the owned group must
        // close its stdout, otherwise a tee reader could outlive the workflow.
        if status.is_err() {
            owned.terminate();
        }
        writer
            .join()
            .map_err(|_| Error::from("media-validation output reader panicked"))??;
        let status = status?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::status(
                status,
                "media-validation harness mode failed",
            ))
        }
    })
}

fn copy_log(mut input: impl Read, output: &mut impl Write, log: &mut impl Write) -> io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            output.flush()?;
            return log.flush();
        }
        output.write_all(&buffer[..count])?;
        log.write_all(&buffer[..count])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tee_preserves_binary_output_across_buffer_boundaries() {
        let bytes: Vec<_> = (0..40_000).map(|index| (index % 256) as u8).collect();
        let (mut output, mut log) = (Vec::new(), Vec::new());
        copy_log(bytes.as_slice(), &mut output, &mut log).unwrap();
        assert_eq!(output, bytes);
        assert_eq!(log, bytes);
    }

    #[test]
    fn tee_reports_a_failed_artifact_writer() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = copy_log(&b"evidence"[..], &mut Vec::new(), &mut Broken).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn failed_mode_is_reaped_and_reported_without_discarding_log() {
        let dir = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        let path = dir.path().join("run.log");
        let mut log = File::create(&path).unwrap();
        log.write_all(b"previous mode\n").unwrap();
        assert!(run_logged(&mut Command::new("/bin/false"), &log).is_err());
        assert_eq!(fs::read(path).unwrap(), b"previous mode\n");
    }
}
