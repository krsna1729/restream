//! Naming of HLS preview resources: the registry key a program preview's
//! segmenter is stored under, the `.hevc` passthrough form of a preview id,
//! and the `__input__:` resource id of an input preview.

const HLS_PREVIEW_KEY_PREFIX: &str = "__preview__:";
const INPUT_PREVIEW_RESOURCE_PREFIX: &str = "__input__:";

pub(crate) fn hls_preview_registry_key(pipeline_id: &str) -> String {
    format!("{HLS_PREVIEW_KEY_PREFIX}{pipeline_id}")
}

pub(crate) fn pipeline_id_from_hls_preview_registry_key(key: &str) -> Option<&str> {
    key.strip_prefix(HLS_PREVIEW_KEY_PREFIX)
}

/// A program preview id with this suffix passes H.265 through, for a browser
/// that decodes it; the plain id gets the shared H.264 bridge. Pipeline ids
/// (`pipeline_<hex>`) never contain a dot.
const HEVC_PASSTHROUGH_PREVIEW_SUFFIX: &str = ".hevc";

pub(crate) fn hevc_passthrough_preview_id(pipeline_id: &str) -> String {
    format!("{pipeline_id}{HEVC_PASSTHROUGH_PREVIEW_SUFFIX}")
}

/// The pipeline a program preview id shows, and whether it passes H.265
/// through.
pub fn preview_pipeline(preview_id: &str) -> (&str, bool) {
    match preview_id.strip_suffix(HEVC_PASSTHROUGH_PREVIEW_SUFFIX) {
        Some(pipeline_id) => (pipeline_id, true),
        None => (preview_id, false),
    }
}

pub fn input_hls_preview_resource_id(input_id: &str) -> String {
    format!("{INPUT_PREVIEW_RESOURCE_PREFIX}{input_id}")
}

/// The input an input-preview resource id shows; a passthrough one
/// (`<input>.hevc`) shows the same input.
pub(crate) fn input_id_from_hls_preview_resource_id(resource_id: &str) -> Option<&str> {
    resource_id
        .strip_prefix(INPUT_PREVIEW_RESOURCE_PREFIX)
        .map(|id| preview_pipeline(id).0)
}

/// The preview with the same source in the other form (H.264 copy versus
/// H.265 as sent): both read one input preview ring.
pub(crate) fn sibling_preview_id(preview_id: &str) -> String {
    match preview_pipeline(preview_id) {
        (base, true) => base.to_string(),
        (base, false) => hevc_passthrough_preview_id(base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_preview_registry_key_roundtrips_through_extraction() {
        let key = hls_preview_registry_key("abc");
        assert_eq!(key, "__preview__:abc");
        assert_eq!(pipeline_id_from_hls_preview_registry_key(&key), Some("abc"));
    }

    #[test]
    fn hls_preview_registry_key_roundtrips_for_empty_pipeline_id() {
        let key = hls_preview_registry_key("");
        assert_eq!(pipeline_id_from_hls_preview_registry_key(&key), Some(""));
    }

    #[test]
    fn pipeline_id_from_hls_preview_registry_key_rejects_unprefixed_key() {
        assert_eq!(pipeline_id_from_hls_preview_registry_key("abc"), None);
        // The prefix must anchor at the start, not just appear somewhere.
        assert_eq!(
            pipeline_id_from_hls_preview_registry_key("notpreview__preview__:abc"),
            None
        );
    }

    // A pipeline id that itself contains the preview prefix as a literal
    // substring only has the outer, registry-added prefix stripped: the
    // extraction is not recursive, so the caller-supplied id survives intact.
    #[test]
    fn hls_preview_registry_key_roundtrips_when_pipeline_id_contains_prefix() {
        let pipeline_id = "__preview__:foo";
        let key = hls_preview_registry_key(pipeline_id);
        assert_eq!(key, "__preview__:__preview__:foo");
        assert_eq!(
            pipeline_id_from_hls_preview_registry_key(&key),
            Some(pipeline_id)
        );
    }
}
