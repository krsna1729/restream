//! Explicit, opt-in generation of transport benchmark and large MSR fixtures.
use crate::workflow::{Error, WorkflowResult, checked};
use std::{fs, path::Path, process::Command};

const USAGE: &str =
    "usage: cargo xtask fixtures generate-bench | generate-msr-1080p [outdir (.local/fixtures)]";

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    match args {
        ["-h" | "--help"] | [_, "-h" | "--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["generate-bench"] => generate_bench(),
        ["generate-msr-1080p", rest @ ..] if rest.len() <= 1 => generate_msr(&crate::user_path(
            rest.first()
                .copied()
                .filter(|s| !s.is_empty())
                .unwrap_or(".local/fixtures"),
        )),
        _ => Err(Error::usage(USAGE)),
    }
}

fn generate_bench() -> WorkflowResult {
    let root = crate::repo_root();
    let out = root.join("test/fixtures/transport");
    fs::create_dir_all(&out)?;
    // The H.264 8m workload is tuned to 8 Mbps of TS payload, not video bitrate.
    for codec in ["h264", "h265"] {
        for two_audio in [false, true] {
            for (label, rate, buffer) in [
                ("1_5m", "1500", "3000"),
                ("4m", "4000", "8000"),
                ("8m", "8000", "16000"),
            ] {
                let (rate, buffer) = if codec == "h264" && label == "8m" {
                    ("6860", "13720")
                } else {
                    (rate, buffer)
                };
                let mut command = bench_command(codec, rate, buffer, two_audio);
                let suffix = if two_audio { "-2a" } else { "" };
                command.arg(out.join(format!("bench-{codec}-{label}{suffix}.ts")));
                checked(&mut command)?;
            }
        }
    }
    Ok(())
}

fn bench_command(codec: &str, rate: &str, buffer: &str, two_audio: bool) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "mandelbrot=size=1920x1080:rate=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
    ]);
    if two_audio {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000",
            "-filter_complex",
            "[2:a]pan=stereo|c0=c0|c1=c0[a2]",
        ]);
    }
    command.args(["-t", "8", "-map", "0:v", "-map", "1:a"]);
    if two_audio {
        command.args(["-map", "[a2]"]);
    }
    if codec == "h264" {
        command.args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-g",
            "60",
            "-bf",
            "2",
            "-b:v",
            &format!("{rate}k"),
            "-maxrate",
            &format!("{rate}k"),
            "-bufsize",
            &format!("{buffer}k"),
        ]);
    } else {
        command.args(["-c:v", "libx265", "-preset", "ultrafast", "-tune", "zerolatency", "-x265-params", &format!("log-level=none:bitrate={rate}:vbv-maxrate={rate}:vbv-bufsize={buffer}:strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1"), "-g", "60", "-bf", "0"]);
    }
    command.args(["-c:a", "aac", "-b:a", "64k", "-f", "mpegts"]);
    command
}

fn generate_msr(outdir: &Path) -> WorkflowResult {
    const SOURCE: &str = "bbb_sunflower_1080p_60fps_normal.mp4";
    const OUTPUT: &str = "bbb-1080p60-30a.mp4";
    fs::create_dir_all(outdir)?;
    if !outdir.join(SOURCE).is_file() {
        checked(Command::new("curl").current_dir(outdir).args(["-fL", "-o", "bbb.zip", "https://ftp.halifax.rwth-aachen.de/blender/demo/movies/BBB/bbb_sunflower_1080p_60fps_normal.mp4"]))?;
        checked(
            Command::new("unzip")
                .current_dir(outdir)
                .args(["-o", "bbb.zip", SOURCE]),
        )?;
        fs::remove_file(outdir.join("bbb.zip"))?;
    }
    checked(Command::new("ffmpeg").current_dir(outdir).args([
        "-v",
        "error",
        "-y",
        "-i",
        SOURCE,
        "-map",
        "0:a:0",
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-ac",
        "2",
        "bbb-audio.m4a",
    ]))?;
    let mut mux = msr_mux_command(SOURCE, OUTPUT);
    checked(mux.current_dir(outdir))?;
    fs::remove_file(outdir.join("bbb-audio.m4a"))?;
    println!("fixture ready: {}/{}", outdir.display(), OUTPUT);
    checked(Command::new("ffprobe").current_dir(outdir).args([
        "-v",
        "error",
        "-show_entries",
        "format=nb_streams,duration,bit_rate",
        "-of",
        "csv=p=0",
        OUTPUT,
    ]))
}

fn msr_mux_command(source: &str, output: &str) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-y",
        "-i",
        source,
        "-i",
        "bbb-audio.m4a",
        "-map",
        "0:v:0",
    ]);
    for _ in 0..30 {
        command.args(["-map", "1:a:0"]);
    }
    command.args(["-c", "copy", "-movflags", "+faststart", output]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }
    fn value<'a>(args: &'a [String], option: &str) -> &'a str {
        let index = args.iter().position(|s| s == option).unwrap();
        &args[index + 1]
    }
    #[test]
    fn bench_shape_retains_bframes_and_audio_selection() {
        let h264 = args(&bench_command("h264", "6860", "13720", true));
        assert_eq!(value(&h264, "-bf"), "2");
        assert_eq!(value(&h264, "-b:v"), "6860k");
        let maps: Vec<_> = h264
            .windows(2)
            .filter(|p| p[0] == "-map")
            .map(|p| p[1].as_str())
            .collect();
        assert_eq!(maps, ["0:v", "1:a", "[a2]"]);
        assert_eq!(
            value(&h264, "-filter_complex"),
            "[2:a]pan=stereo|c0=c0|c1=c0[a2]"
        );
        let h265 = args(&bench_command("h265", "8000", "16000", false));
        assert_eq!(value(&h265, "-bf"), "0");
        assert!(
            value(&h265, "-x265-params")
                .contains("strict-cbr=1:keyint=60:min-keyint=60:no-scenecut=1")
        );
    }
    #[test]
    fn msr_copies_one_video_and_thirty_audio_tracks() {
        let args = args(&msr_mux_command(
            "source with spaces.mp4",
            "output with spaces.mp4",
        ));
        let maps: Vec<_> = args
            .windows(2)
            .filter(|p| p[0] == "-map")
            .map(|p| p[1].as_str())
            .collect();
        assert_eq!(maps[0], "0:v:0");
        assert_eq!(maps[1..], vec!["1:a:0"; 30]);
        assert_eq!(value(&args, "-c"), "copy");
        assert_eq!(args.last().unwrap(), "output with spaces.mp4");
    }
}
