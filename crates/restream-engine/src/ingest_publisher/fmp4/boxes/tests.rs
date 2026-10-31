#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use proptest::prelude::*;

/// A box with a 32-bit size.
pub(crate) fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A box with size 1 and a 64-bit `largesize` after the type.
fn large(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(&u64::try_from(body.len() + 16).unwrap().to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// A `moof` holding one `traf`, with a `tfhd` of `flags` and (`with_tfdt`)
/// a `tfdt` at decode time 0.
fn moof_with(flags: u32, with_tfdt: bool) -> Vec<u8> {
    let mut tfhd = vec![0u8];
    tfhd.extend_from_slice(&flags.to_be_bytes()[1..]);
    tfhd.extend_from_slice(&1u32.to_be_bytes());
    if flags & 1 != 0 {
        tfhd.extend_from_slice(&0u64.to_be_bytes());
    }
    let mut children = vec![bx(b"tfhd", &tfhd)];
    if with_tfdt {
        let mut tfdt = vec![0u8];
        tfdt.extend_from_slice(&0u32.to_be_bytes());
        children.push(bx(b"tfdt", &tfdt));
    }
    let traf = bx(b"traf", &children.concat());
    bx(b"moof", &[bx(b"mfhd", &[0; 8]), traf].concat())
}

/// A `moof` whose `traf` carries a `tfdt`: every fragment the walker
/// accepts has one.
fn moof(flags: u32) -> Vec<u8> {
    moof_with(flags, true)
}

fn walk(bytes: &[u8]) -> Result<(Vec<Range<usize>>, WalkReport), WalkError> {
    let mut fragments = Vec::new();
    media_fragments(bytes, &mut fragments).map(|report| (fragments, report))
}

/// FFmpeg's layout: `styp sidx moof mdat`, one fragment, nothing unknown.
#[test]
fn ffmpeg_segment_layout_yields_its_one_fragment() {
    let head = [bx(b"styp", &[0; 16]), bx(b"sidx", &[0; 44])].concat();
    let fragment = [moof(0x02_0000), bx(b"mdat", &[7; 32])].concat();
    let segment = [head.clone(), fragment.clone()].concat();
    let (fragments, report) = walk(&segment).unwrap();
    assert_eq!(fragments, vec![head.len()..segment.len()]);
    assert_eq!(report.first_unknown, None);
}

#[test]
fn several_fragments_are_walked_in_order_and_bounded() {
    let fragment = [moof(0), bx(b"mdat", &[1; 4])].concat();
    let segment = [fragment.clone(), bx(b"free", &[]), fragment.clone()].concat();
    let (fragments, _) = walk(&segment).unwrap();
    let second = fragment.len() + 8;
    assert_eq!(
        fragments,
        vec![0..fragment.len(), second..second + fragment.len()]
    );

    let at_bound = fragment.repeat(MAX_FRAGMENTS);
    assert_eq!(walk(&at_bound).unwrap().0.len(), MAX_FRAGMENTS);
    let over = fragment.repeat(MAX_FRAGMENTS + 1);
    assert_eq!(walk(&over), Err(WalkError::TooManyFragments));
}

/// Unknown top-level boxes are skipped; the first is reported once.
#[test]
fn unknown_boxes_are_skipped_and_the_first_reported() {
    let segment = [
        bx(b"zzzz", &[1, 2, 3]),
        bx(b"prft", &[0; 12]),
        moof(0),
        bx(b"mdat", &[]),
        bx(b"yyyy", &[]),
    ]
    .concat();
    let (fragments, report) = walk(&segment).unwrap();
    assert_eq!(fragments.len(), 1);
    assert_eq!(report.first_unknown, Some(*b"zzzz"));
}

#[test]
fn size_one_reads_a_64_bit_largesize() {
    let segment = [large(b"styp", &[0; 4]), moof(0), large(b"mdat", &[9; 20])].concat();
    let (fragments, _) = walk(&segment).unwrap();
    assert_eq!(fragments, vec![20..segment.len()]);
}

#[test]
fn size_zero_runs_to_the_end_only_for_skippable_boxes() {
    let fragment = [moof(0), bx(b"mdat", &[1; 4])].concat();
    let mut trailing_free = fragment.clone();
    trailing_free.extend_from_slice(&[0, 0, 0, 0]);
    trailing_free.extend_from_slice(b"free");
    trailing_free.extend_from_slice(&[5; 10]);
    assert_eq!(walk(&trailing_free).unwrap().0, vec![0..fragment.len()]);

    let mut mdat_to_end = moof(0);
    mdat_to_end.extend_from_slice(&[0, 0, 0, 0]);
    mdat_to_end.extend_from_slice(b"mdat");
    assert_eq!(
        walk(&mdat_to_end),
        Err(WalkError::SizeToEnd { kind: *b"mdat" })
    );

    let mut moof_to_end = vec![0, 0, 0, 0];
    moof_to_end.extend_from_slice(b"moof");
    assert_eq!(
        walk(&moof_to_end),
        Err(WalkError::SizeToEnd { kind: *b"moof" })
    );
}

#[test]
fn malformed_layouts_are_refused() {
    let fragment = [moof(0), bx(b"mdat", &[1; 4])].concat();
    // Truncated: the object ends inside a header, or inside a box.
    assert_eq!(
        walk(&fragment[..fragment.len() - 2]),
        Err(WalkError::Oversized {
            kind: *b"mdat",
            offset: moof(0).len()
        })
    );
    let mut cut_header = fragment.clone();
    cut_header.extend_from_slice(&[0, 0, 0]);
    assert_eq!(
        walk(&cut_header),
        Err(WalkError::Truncated {
            offset: fragment.len()
        })
    );
    // A size below the header.
    let mut small = vec![0, 0, 0, 7];
    small.extend_from_slice(b"styp");
    assert_eq!(
        walk(&small),
        Err(WalkError::TooSmall {
            kind: *b"styp",
            offset: 0
        })
    );
    let mut small_large = 1u32.to_be_bytes().to_vec();
    small_large.extend_from_slice(b"mdat");
    small_large.extend_from_slice(&15u64.to_be_bytes());
    assert_eq!(
        walk(&small_large),
        Err(WalkError::TooSmall {
            kind: *b"mdat",
            offset: 0
        })
    );
    // An oversized largesize that overflows any offset.
    let mut huge = 1u32.to_be_bytes().to_vec();
    huge.extend_from_slice(b"free");
    huge.extend_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(
        walk(&huge),
        Err(WalkError::Oversized {
            kind: *b"free",
            offset: 0
        })
    );
    // Pairing.
    assert_eq!(
        walk(&moof(0)),
        Err(WalkError::MoofWithoutMdat { offset: 0 })
    );
    assert_eq!(
        walk(&[moof(0), bx(b"free", &[]), bx(b"mdat", &[])].concat()),
        Err(WalkError::MoofWithoutMdat { offset: 0 })
    );
    assert_eq!(
        walk(&bx(b"mdat", &[1])),
        Err(WalkError::MdatWithoutMoof { offset: 0 })
    );
    assert_eq!(walk(&bx(b"styp", &[0; 8])), Err(WalkError::NoFragments));
    assert_eq!(walk(&[]), Err(WalkError::NoFragments));
}

/// `tfdt` is required: without it the demuxer would place every fragment at
/// decode time 0 and glue a restarted timeline onto the previous one.
#[test]
fn a_traf_without_a_tfdt_is_refused() {
    let fragment = [moof_with(0x02_0000, false), bx(b"mdat", &[0; 4])].concat();
    assert_eq!(walk(&fragment), Err(WalkError::TrafWithoutTfdt));

    let fragment = [moof_with(0x02_0000, true), bx(b"mdat", &[0; 4])].concat();
    assert!(walk(&fragment).is_ok());
}

/// Offsets counted from the whole segment would point every sample at the
/// wrong bytes once the walker hands the demuxer one fragment; the
/// moof-relative forms (`default_base_is_moof`, or no flag) are accepted.
#[test]
fn an_explicit_base_data_offset_is_refused() {
    let explicit = [bx(b"styp", &[0; 8]), moof(0x00_0001), bx(b"mdat", &[0; 8])].concat();
    assert_eq!(walk(&explicit), Err(WalkError::ExplicitBaseDataOffset));
    let both = [moof(0x02_0001), bx(b"mdat", &[0; 8])].concat();
    assert_eq!(walk(&both), Err(WalkError::ExplicitBaseDataOffset));
    for accepted in [0x02_0000, 0x00_0000, 0x00_0020] {
        let segment = [moof(accepted), bx(b"mdat", &[0; 8])].concat();
        assert!(walk(&segment).is_ok(), "tfhd flags {accepted:#x}");
    }
}

#[test]
fn init_segments_are_recognised_by_their_moov() {
    let init = [bx(b"ftyp", &[0; 20]), bx(b"moov", &[0; 40])].concat();
    assert!(is_init_segment(&init));
    assert_eq!(moov_range(&init), Ok(28..init.len()));
    let media = [bx(b"styp", &[0; 8]), moof(0), bx(b"mdat", &[])].concat();
    assert!(!is_init_segment(&media));
    assert_eq!(moov_range(&media), Err(WalkError::NoMoov));
    assert!(!is_init_segment(&[0, 0, 0, 3]));
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let mut fragments = Vec::new();
        if media_fragments(&bytes, &mut fragments).is_ok() {
            for fragment in &fragments {
                prop_assert!(fragment.end <= bytes.len());
            }
        }
        let _ = is_init_segment(&bytes);
        let _ = moov_range(&bytes);
    }
}

/// A fragment whose `moof` is padded to `moof_bytes` in total.
fn fragment_with_moof_of(moof_bytes: usize) -> Vec<u8> {
    let children = moof(0)[8..].to_vec();
    let pad = bx(b"free", &vec![0; moof_bytes - 16 - children.len()]);
    [bx(b"moof", &[children, pad].concat()), bx(b"mdat", &[0; 4])].concat()
}

/// The demuxer decodes a whole `moof` in one poll, so its size is bounded.
#[test]
fn a_moof_past_its_size_bound_is_refused() {
    assert!(walk(&fragment_with_moof_of(MAX_MOOF_BYTES)).is_ok());
    assert_eq!(
        walk(&fragment_with_moof_of(MAX_MOOF_BYTES + 1)).unwrap_err(),
        WalkError::MoofTooLarge { offset: 0 }
    );
}

/// A fragment whose one `traf` declares `count` samples in one `trun` of
/// zero-byte entries (every field from defaults).
fn fragment_with_samples(count: u32) -> Vec<u8> {
    let tfhd = bx(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]);
    let tfdt = bx(b"tfdt", &[0; 8]);
    let mut trun = vec![0u8; 4];
    trun.extend_from_slice(&count.to_be_bytes());
    let traf = bx(b"traf", &[tfhd, tfdt, bx(b"trun", &trun)].concat());
    let moof = bx(b"moof", &[bx(b"mfhd", &[0; 8]), traf].concat());
    [moof, bx(b"mdat", &[0; 4])].concat()
}

/// Zero-byte `trun` entries make the sample count independent of the
/// `moof` size, so the segment's declared samples are bounded on their own,
/// summed across its fragments.
#[test]
fn a_segment_declaring_too_many_samples_is_refused() {
    let half = u32::try_from(MAX_SEGMENT_SAMPLES / 2).unwrap();
    let at_bound = [fragment_with_samples(half), fragment_with_samples(half)].concat();
    assert!(walk(&at_bound).is_ok());
    let over = [fragment_with_samples(half), fragment_with_samples(half + 1)].concat();
    assert_eq!(walk(&over).unwrap_err(), WalkError::TooManySamples);
}
