//! Writes moq-net's generated wire seeds (`moq_net::fuzz::seeds`) for the
//! decoders the `moq_wire` target drives into the directory given as the
//! only argument: one valid input per draft and message type, so fuzzing
//! starts past the version and type selectors.

fn main() -> std::io::Result<()> {
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| std::io::Error::other("usage: moq-wire-seeds <corpus dir>"))?;
    std::fs::create_dir_all(&out)?;
    let mut written = 0;
    for (index, seed) in moq_net::fuzz::seeds().into_iter().enumerate() {
        if matches!(seed.target, "ietf" | "lite" | "announce") {
            std::fs::write(out.join(format!("seed-{}-{index}", seed.target)), seed.data)?;
            written += 1;
        }
    }
    if written == 0 {
        return Err(std::io::Error::other("moq-net generated no wire seeds"));
    }
    Ok(())
}
