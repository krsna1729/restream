//! Fuzz corpus seeding and the CI fuzz smoke; never called by ordinary
//! fixture consumers.
use crate::workflow::{Error, TempDir, WorkflowResult, checked, output};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

mod seeds;

/// Writes the seed corpus every target starts from.
pub(crate) fn seed_corpus() -> WorkflowResult {
    let root = crate::repo_root();
    let fuzz = root.join("fuzz");
    let corpus = fuzz.join("corpus");
    let transport = root.join("test/fixtures/transport");
    seed_transport(&transport, &corpus.join("ts_demux"))?;
    seeds::http(&corpus.join("http_response"))?;
    seeds::hls(&corpus.join("hls_playlist"))?;
    seed_fmp4(&transport, &corpus.join("fmp4_ingest"))?;
    seeds::moq(&corpus.join("moq_ingest"))?;
    // This must remain the real helper: it uses the pinned moq-net wire codecs.
    checked(Command::new("cargo").current_dir(fuzz).args([
        "run",
        "--quiet",
        "--manifest-path",
        "moq-seeds/Cargo.toml",
        "--",
        "corpus/moq_wire",
    ]))
}

/// Runs every target `cargo +nightly fuzz list` reports for 30 s each.
pub(crate) fn smoke() -> WorkflowResult {
    let fuzz = crate::repo_root().join("fuzz");
    let listed = output(
        Command::new("cargo")
            .current_dir(&fuzz)
            .args(["+nightly", "fuzz", "list"]),
    )?;
    let listed = String::from_utf8_lossy(&listed.stdout);
    let targets: Vec<&str> = listed.split_whitespace().collect();
    if targets.is_empty() {
        return Err("cargo fuzz list reported no targets".into());
    }
    for target in targets {
        eprintln!("fuzz target {target}");
        checked(Command::new("cargo").current_dir(&fuzz).args([
            "+nightly",
            "fuzz",
            "run",
            target,
            "--",
            "-max_total_time=30",
            "-timeout=5",
        ]))?;
    }
    Ok(())
}

fn matching_files(dir: &Path, prefix: &str, suffix: &str) -> Result<Vec<PathBuf>, Error> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with(prefix) && name.ends_with(suffix))
        {
            paths.push(path);
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no {prefix}*{suffix} files in {}", dir.display()).into());
    }
    Ok(paths)
}

fn seed_transport(transport: &Path, out: &Path) -> WorkflowResult {
    fs::create_dir_all(out)?;
    for path in matching_files(transport, "av-marker-", ".ts")? {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("non-UTF-8 fixture name")?;
        // Read only twenty TS packets; a smaller fixture is copied to its actual end.
        let mut payload = Vec::with_capacity(3760);
        fs::File::open(&path)?
            .take(3760)
            .read_to_end(&mut payload)?;
        for mode in [0x01, 0x0e] {
            let mut seed = Vec::with_capacity(payload.len() + 1);
            seed.push(mode);
            seed.extend_from_slice(&payload);
            fs::write(out.join(format!("seed-{name}-{mode:02x}")), seed)?;
        }
    }
    Ok(())
}

fn seed_fmp4(transport: &Path, out: &Path) -> WorkflowResult {
    fs::create_dir_all(out)?;
    let temp = TempDir::new("restream-fuzz-seeds")?;
    let work = temp.path();
    checked(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(transport.join("correctness-h264.ts"))
            .args([
                "-t",
                "1",
                "-vf",
                "scale=64:36",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-bf",
                "2",
                "-g",
                "6",
                "-c:a",
                "aac",
                "-b:a",
                "24k",
                "-f",
                "hls",
                "-hls_segment_type",
                "fmp4",
                "-hls_time",
                "0.2",
                "-hls_list_size",
                "0",
            ])
            .arg(work.join("index.m3u8")),
    )?;
    let init = fs::read(work.join("init.mp4"))?;
    for segment in matching_files(work, "index", ".m4s")? {
        let name = segment
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("non-UTF-8 segment name")?;
        let media = fs::read(&segment)?;
        let mut seed = Vec::with_capacity(2 + init.len() + media.len());
        seeds::length_prefixed(&mut seed, &init)?;
        seed.extend_from_slice(&media);
        fs::write(out.join(format!("seed-{name}")), seed)?;
    }
    Ok(())
}
