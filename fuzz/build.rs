//! `cargo:rustc-link-arg` from the root `build.rs` applies only to the
//! restream package's own binaries, not to these fuzz binaries that link the
//! library, so repeat its static FFmpeg archive group (see
//! `emit_ffmpeg_static_archive_group` in ../build.rs) for GNU ld's one-pass
//! archive resolution.

fn main() {
    println!("cargo:rustc-link-arg=-Wl,-Bstatic");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    for library in [
        "avfilter",
        "avformat",
        "avcodec",
        "swscale",
        "swresample",
        "avutil",
        "x264",
        "x265",
        "stdc++",
        "gcc",
        "rt",
        "dl",
        "m",
        "atomic",
        "pthread",
    ] {
        println!("cargo:rustc-link-arg=-l{library}");
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    println!("cargo:rustc-link-arg=-Wl,-Bdynamic");
}
