//! Pipeline inputs in the runtime state: the stored input and what its
//! session is doing, without the counters a session advances on every read
//! (uptime, bytes, transport quality), which `/api/v1/engine/telemetry`
//! samples instead.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::config::ServerPorts;
use crate::media::engine::MediaEngine;
use crate::media::input_gate::InputForwardState;
use restream_domain::domain::pipeline_input::{
    PipelineInput, PipelineInputRole, PipelineInputSourceKind, VariantRule,
};

/// Where a keyed input's publisher pushes. A pull input accepts no pushes,
/// so it has none.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct IngestUrls {
    pub rtmp: String,
    pub srt: String,
    /// HLS push (`-f hls -method PUT|POST`): the playlist URL; the segments
    /// go next to it.
    pub hls: String,
    /// MoQ publish (`https://`: WebTransport or QUIC, moq.dev convention): the
    /// last path segment is the stream key. `None`
    /// while the MoQ ingest listener is off.
    pub moq: Option<String>,
}

/// The host and listener ports an input's publish URLs name.
pub struct IngestEndpoints {
    pub host: String,
    pub ports: ServerPorts,
}

impl IngestEndpoints {
    pub fn urls(&self, input: &PipelineInput) -> Option<IngestUrls> {
        let Self { host, ports } = self;
        let key = &input.stream_key;
        match input.source_kind {
            PipelineInputSourceKind::Keyed => Some(IngestUrls {
                rtmp: format!("rtmp://{host}:{}/live/{key}", ports.rtmp),
                srt: format!("srt://{host}:{}?streamid=publish:{key}", ports.srt),
                hls: format!("http://{host}:{}/ingest/hls/{key}/index.m3u8", ports.http),
                moq: ports.moq.map(|port| format!("https://{host}:{port}/{key}")),
            }),
            PipelineInputSourceKind::HlsPull | PipelineInputSourceKind::SrtCall => None,
        }
    }
}

/// The input's own HLS preview.
pub fn input_preview_url(input_id: &str) -> String {
    format!("/hls/inputs/{input_id}/master.m3u8")
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InputState {
    pub id: String,
    pub pipeline_id: String,
    /// Where the input sits in its pipeline's list: the primary first, then
    /// the backups in the order they were added.
    pub position: u32,
    pub label: String,
    pub stream_key: String,
    pub role: PipelineInputRole,
    pub enabled: bool,
    pub selected: bool,
    pub kind: PipelineInputSourceKind,
    /// The remote origin, for a pull input only.
    pub source_url: Option<String>,
    pub variant_rule: VariantRule,
    /// Auth is write-only: the variant and (for basic) the username are safe
    /// to show, but no token, password or header value ever is.
    #[cfg_attr(
        test,
        ts(type = "\"none\" | \"bearer\" | \"cookies\" | \"basic\" | \"headers\"")
    )]
    pub auth_kind: String,
    pub auth_username: Option<String>,
    /// HLS push credentials are write-only, independently of pull auth.
    #[cfg_attr(test, ts(type = "\"none\" | \"basic\""))]
    pub push_auth_kind: String,
    pub push_auth_username: Option<String>,
    pub ingest_urls: Option<IngestUrls>,
    pub preview_url: String,
    pub runtime: InputRuntimeState,
}

/// The input's session; `connected: false` and nothing else without one.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InputRuntimeState {
    pub connected: bool,
    pub forwarding_state: Option<InputForwardState>,
    pub protocol: Option<String>,
    pub remote_addr: Option<String>,
    pub video: Option<VideoDescriptor>,
    pub audio: Option<AudioDescriptor>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct VideoDescriptor {
    pub codec: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AudioDescriptor {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Projects every input row with its session. `inputs` come in list order
/// (by pipeline, then primary first and backups by age), which sets each
/// input's `position`.
pub(super) async fn project_inputs(
    engine: &MediaEngine,
    inputs: &[PipelineInput],
    endpoints: &IngestEndpoints,
) -> BTreeMap<String, InputState> {
    let sessions: HashMap<String, InputRuntimeState> = engine
        .ingests
        .sessions
        .read()
        .await
        .iter()
        .map(|(input_id, session)| {
            let metadata = session.metadata();
            (
                input_id.clone(),
                InputRuntimeState {
                    connected: true,
                    forwarding_state: Some(session.gate.state()),
                    protocol: Some(session.protocol.clone()),
                    remote_addr: metadata.remote_addr,
                    video: metadata.video.map(|video| VideoDescriptor {
                        codec: video.codec,
                        width: video.width,
                        height: video.height,
                    }),
                    audio: metadata.audio.map(|audio| AudioDescriptor {
                        codec: audio.codec,
                        sample_rate: audio.sample_rate,
                        channels: audio.channels,
                    }),
                },
            )
        })
        .collect();
    let mut positions: HashMap<&str, u32> = HashMap::new();
    inputs
        .iter()
        .map(|input| {
            let position = positions.entry(input.pipeline_id.as_str()).or_default();
            let state = InputState {
                id: input.id.clone(),
                pipeline_id: input.pipeline_id.clone(),
                position: *position,
                label: input.label.clone(),
                stream_key: input.stream_key.clone(),
                role: input.role,
                enabled: input.enabled,
                selected: input.selected,
                kind: input.source_kind,
                source_url: input.source_url.clone(),
                variant_rule: input.variant_rule,
                auth_kind: input.source_auth.kind_str().to_string(),
                auth_username: input.source_auth.username().map(str::to_string),
                push_auth_kind: input.push_auth.kind_str().to_string(),
                push_auth_username: input.push_auth.username().map(str::to_string),
                ingest_urls: endpoints.urls(input),
                preview_url: input_preview_url(&input.id),
                runtime: sessions.get(&input.id).cloned().unwrap_or_default(),
            };
            *position += 1;
            (input.id.clone(), state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_domain::domain::pipeline_input::PushAuth;

    #[tokio::test]
    async fn push_auth_runtime_exposes_state_not_credentials_or_urls() {
        let engine = MediaEngine::new();
        let input = PipelineInput {
            id: "input".to_string(),
            pipeline_id: "pipeline".to_string(),
            stream_key: "key".to_string(),
            push_auth: PushAuth::Basic {
                username: "encoder".to_string(),
                password_hash: "secret-derived-password-hash".to_string(),
            },
            ..PipelineInput::keyed_default()
        };
        let endpoints = IngestEndpoints {
            host: "localhost".to_string(),
            ports: crate::AppConfig::default().ports,
        };
        let state = project_inputs(&engine, &[input], &endpoints).await;
        assert_eq!(state["input"].push_auth_kind, "basic");
        assert_eq!(
            state["input"].push_auth_username.as_deref(),
            Some("encoder")
        );
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains("secret-derived-password-hash"));
        assert!(!serialized.contains("password"));
        let urls = serde_json::to_string(&state["input"].ingest_urls).unwrap();
        assert!(!urls.contains("encoder"));
        assert!(!urls.contains('@'));
    }
}
