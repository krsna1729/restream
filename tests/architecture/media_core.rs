#[test]
fn candidate_types_stay_independent_of_engine_and_runtime_adapters() {
    for (name, source) in [
        (
            "restream-core/lib.rs",
            include_str!("../../crates/restream-core/src/lib.rs"),
        ),
        (
            "restream-core/metadata.rs",
            include_str!("../../crates/restream-core/src/metadata.rs"),
        ),
    ] {
        for forbidden in [
            "restream_engine::",
            "ffmpeg",
            "srt",
            "tokio",
            "axum",
            "sqlx",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} must stay independent of {forbidden}"
            );
        }
    }
}
