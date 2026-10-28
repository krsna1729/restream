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
            "crate::media::engine",
            "crate::media::ring_buffer",
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

#[test]
fn ring_metadata_depends_on_metadata_owner_instead_of_engine() {
    let ring_buffer = include_str!("../../crates/restream-sync/src/ring_buffer.rs");
    assert!(!ring_buffer.contains("crate::media::engine"));
    assert!(!ring_buffer.contains("crate::media::engine::AudioMeta"));
    assert!(ring_buffer.contains("restream_core"));
    assert!(ring_buffer.contains("AudioMeta"));
}
