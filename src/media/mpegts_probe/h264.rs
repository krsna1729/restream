use super::for_each_nal_raw;
use crate::media::metadata::VideoMeta;

#[inline]
pub(in crate::media::mpegts) fn is_keyframe(payload: &[u8]) -> bool {
    for_each_nal(payload, |nal_type, _nal_data| {
        // NAL type 5 = IDR slice
        if nal_type == 5 {
            return true;
        }
        false
    })
}

pub(in crate::media::mpegts) fn find_sps(payload: &[u8]) -> Option<Vec<u8>> {
    let mut result = None;
    for_each_nal_raw(payload, |nal_data| match nal_data.split_first() {
        Some((&header, payload)) if header & 0x1F == 7 => {
            result = Some(payload.to_vec());
            true
        }
        _ => false,
    });
    result
}

fn for_each_nal<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(u8, &[u8]) -> bool,
{
    for_each_nal_raw(data, |nal_data| {
        nal_data
            .split_first()
            .is_some_and(|(&header, payload)| callback(header & 0x1f, payload))
    })
}

/// Fills `meta` from an SPS NAL payload (after the NAL header) through the
/// parser shared with RTMP ingest.
pub(super) fn parse_sps(raw_sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
    let sps = crate::media::codec::parse_h264_sps(raw_sps)?;
    meta.profile = Some(crate::media::codec::profile_name(sps.profile_idc).to_string());
    meta.level = Some(crate::media::codec::level_name(sps.level_idc));
    meta.width = sps.width;
    meta.height = sps.height;
    if sps.fps > 0.0 {
        meta.fps = sps.fps;
    }
    Some(())
}
