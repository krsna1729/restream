use super::*;
use crate::domain::stage::StageKind;
use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::ffmpeg::stage_plan::{StageInputSpec, VideoStageOp};
use crate::media::metadata::AudioMeta;
use crate::media::packet::PayloadFormat;
use crate::media::ring_buffer::Reader;
use proptest::prelude::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

include!("tests/routing.rs");
include!("tests/runtime.rs");
