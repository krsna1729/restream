use super::*;

/// Listings saved from the BtbN build `restream ffmpeg-fetch` pins.
fn btbn() -> Components {
    let mut components = Components::default();
    for (kind, listing) in [
        (
            Kind::Encoder,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/encoders.txt"),
        ),
        (
            Kind::Decoder,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/decoders.txt"),
        ),
        (
            Kind::Muxer,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/muxers.txt"),
        ),
        (
            Kind::Demuxer,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/demuxers.txt"),
        ),
        (
            Kind::Filter,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/filters.txt"),
        ),
        (
            Kind::Protocol,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/protocols.txt"),
        ),
        (
            Kind::BitstreamFilter,
            include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/bsfs.txt"),
        ),
    ] {
        components.read(kind, listing);
    }
    components
}

#[test]
fn the_pinned_build_provides_every_feature() {
    let features = judge(Some(&btbn()), true);
    assert_eq!(features.len(), Feature::ALL.len());
    for status in features {
        assert!(
            status.available,
            "{:?} lacks {:?}",
            status.feature, status.missing
        );
    }
}

#[test]
fn listings_are_read_by_alias_and_by_direction() {
    let components = btbn();
    // `mov,mp4,m4a,3gp,3g2,mj2` is one demuxer listed under all its aliases.
    assert!(components.has(Kind::Demuxer, "mp4"));
    assert!(components.has(Kind::Demuxer, "mov"));
    assert!(components.has(Kind::Encoder, "libx265"));
    assert!(components.has(Kind::Protocol, "unix"));
    assert!(components.has(Kind::BitstreamFilter, "aac_adtstoasc"));
    // Header and legend lines are not components.
    assert!(!components.has(Kind::Encoder, "V....."));
    assert!(!components.has(Kind::Protocol, "Output:"));

    // A format line counts as a muxer only with the E flag.
    let mut formats = Components::default();
    formats.read(
        Kind::Muxer,
        "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ---\n D  mpegtsraw raw\n  E mpegts MPEG-TS\n",
    );
    assert!(formats.has(Kind::Muxer, "mpegts"));
    assert!(!formats.has(Kind::Muxer, "mpegtsraw"));
}

#[test]
fn a_build_without_libx265_loses_only_transcoding() {
    let full = include_str!("../../../../test/fixtures/ffmpeg/btbn-n9.0.2/encoders.txt");
    let without: String = full
        .lines()
        .filter(|line| !line.contains(" libx265 "))
        .map(|line| format!("{line}\n"))
        .collect();
    let mut components = btbn();
    components.names[Kind::Encoder as usize].clear();
    components.read(Kind::Encoder, &without);

    for status in judge(Some(&components), true) {
        if status.feature == Feature::Transcoding {
            assert!(!status.available);
            assert_eq!(status.missing, ["encoder libx265"]);
        } else {
            assert!(status.available, "{:?}", status.feature);
        }
    }
}

#[test]
fn a_missing_executable_is_named_on_the_features_it_disables() {
    for status in judge(None, false) {
        let want = if status.feature == Feature::MediaAnalysis {
            "ffprobe"
        } else {
            "ffmpeg"
        };
        assert_eq!(status.missing, [want], "{:?}", status.feature);
    }
}

#[test]
fn the_version_comes_from_the_banner() {
    assert_eq!(
        version_from_banner(include_str!(
            "../../../../test/fixtures/ffmpeg/btbn-n9.0.2/version.txt"
        ))
        .as_deref(),
        Some("n9.0.2-17-g2a571b6068-20260930")
    );
    assert_eq!(version_from_banner("garbage"), None);
}

#[tokio::test]
async fn probing_a_missing_executable_reports_it_without_panicking() {
    let report = probe(
        Path::new("/nonexistent/ffmpeg"),
        Path::new("/nonexistent/ffprobe"),
    )
    .await;
    assert!(report.ffmpeg.version.is_none() && report.ffmpeg.error.is_some());
    assert!(
        Feature::ALL
            .iter()
            .all(|&feature| !report.available(feature))
    );
}

#[test]
fn the_older_format_listing_layout_is_read_too() {
    // FFmpeg 6.x ends the format legend with `--`, 8.x with `---`.
    let mut components = Components::default();
    components.read(
        Kind::Muxer,
        include_str!("../../../../test/fixtures/ffmpeg/ubuntu-6.1/muxers.txt"),
    );
    assert!(components.has(Kind::Muxer, "mov"));
    assert!(components.has(Kind::Muxer, "mpegts"));
    assert!(!components.has(Kind::Muxer, "D."));
}
