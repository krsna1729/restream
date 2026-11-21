//! Input health: publisher, signal quality and reader pressure.

use super::{
    Alert, BLACK_ALERT_MS, Evidence, LAG_SLOTS_WARN, SILENT_ALERT_MS, SRT_RECV_BUFFER_CRITICAL_PCT,
    SRT_RECV_BUFFER_WARN_PCT, STILL_ALERT_MS, Scope, Severity,
};

pub(super) fn push_alerts(
    pipeline_id: &str,
    input: &serde_json::Value,
    input_status: &str,
    generated_at: &str,
    alerts: &mut Vec<Alert>,
) {
    if input_status == "off" {
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:no_publisher", pipeline_id),
                Severity::Critical,
                Scope::Pipeline,
                "No active publisher",
                "The pipeline is configured but not receiving a stream.",
                vec![Evidence::measured("input.status = off")],
                "Start the publisher or check the stream key and connection.",
                generated_at,
            )
        });
    }

    let srt_recv_buffer = input
        .get("publisher")
        .and_then(|publisher| publisher.get("quality"))
        .and_then(srt_recv_buffer_occupancy);
    if let Some((recv_packets, capacity_packets, pct)) = srt_recv_buffer
        && pct >= SRT_RECV_BUFFER_WARN_PCT
    {
        let critical = pct >= SRT_RECV_BUFFER_CRITICAL_PCT;
        let severity = if critical {
            Severity::Critical
        } else {
            Severity::Warning
        };
        let title = if critical {
            "SRT publisher ingest is not being drained"
        } else {
            "SRT publisher ingest receive buffer is filling"
        };
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:input:srt_recv_buffer_saturated", pipeline_id),
                severity,
                Scope::Pipeline,
                title,
                "The SRT application receive buffer is full or nearly full. The publisher can still be connected while Restream is not draining ingest data, so downstream outputs will stall. The kernel UDP queue may still look empty: the packets have already entered the srt-rs Owner.",
                vec![
                    Evidence::measured(format!(
                        "srtRecvBufPackets = {} / {} ({:.0}%)",
                        recv_packets, capacity_packets, pct
                    )),
                ],
                "Treat this as an input/ingest issue first: restart the affected publisher or Restream, then inspect SRT ingest readiness if it recurs.",
                generated_at,
            )
        });
    }

    if let Some(qa) = input.get("qa") {
        push_signal_alerts(pipeline_id, qa, generated_at, alerts);
    }

    // Per-reader: lag and overflow
    if let Some(readers) = input.get("readerMetrics").and_then(|v| v.as_array()) {
        for reader in readers {
            let name = reader
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let lag = reader.get("lagSlots").and_then(|v| v.as_u64()).unwrap_or(0);
            if lag > LAG_SLOTS_WARN {
                alerts.push(Alert {
                    pipeline_id: Some(pipeline_id.to_owned()),
                    stage_id: Some(name.to_string()),
                    ..Alert::new(
                        format!("pipeline:{}:stage:{}:lag", pipeline_id, name),
                        Severity::Warning,
                        Scope::Stage,
                        format!("Stage '{}' is lagging behind the ring buffer", name),
                        "The consumer is reading slower than the producer is writing.",
                        vec![Evidence::measured(format!(
                            "lagSlots = {} (threshold {})",
                            lag, LAG_SLOTS_WARN
                        ))],
                        "Check downstream network/encoder throughput or reduce output bitrate.",
                        generated_at,
                    )
                });
            }

            let overflows = reader
                .get("overflowCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if overflows > 0 {
                alerts.push(Alert {
                    pipeline_id: Some(pipeline_id.to_owned()),
                    stage_id: Some(name.to_string()),
                    ..Alert::new(
                        format!("pipeline:{}:stage:{}:overflow", pipeline_id, name),
                        Severity::Warning,
                        Scope::Stage,
                        format!(
                            "Stage '{}' has overflowed the ring buffer {} time(s)",
                            name, overflows
                        ),
                        "The ring buffer was full when this reader tried to consume packets; \
                            some packets were skipped.",
                        vec![Evidence::measured(format!("overflowCount = {}", overflows))],
                        "Reduce output count or increase processing throughput.",
                        generated_at,
                    )
                });
            }
        }
    }
}

fn push_signal_alerts(
    pipeline_id: &str,
    qa: &serde_json::Value,
    generated_at: &str,
    alerts: &mut Vec<Alert>,
) {
    let held = |key: &str| qa.get(key).and_then(|v| v.as_u64());
    if let Some(ms) = held("blackForMs")
        && ms >= BLACK_ALERT_MS
    {
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:input:picture_black", pipeline_id),
                Severity::Critical,
                Scope::Pipeline,
                "Picture is black",
                "The source is connected and sending, but its picture is black, so every destination shows black.",
                vec![Evidence::measured(format!("black for {} s", ms / 1000))],
                "Check the encoder's scene, camera or capture input; the stream itself is fine.",
                generated_at,
            )
        });
    } else if let Some(ms) = held("stillForMs")
        && ms >= STILL_ALERT_MS
    {
        let slate = qa.get("stillIsSlate").and_then(|v| v.as_bool()) == Some(true);
        let (id, title, summary, fix) = if slate {
            (
                "picture_slate",
                "A slate is on air",
                "The picture has stayed the same and looks drawn (bars, a card or a still graphic): viewers see a slate, not the program.",
                "If the slate is not planned, switch the encoder back to the program scene.",
            )
        } else {
            (
                "picture_frozen",
                "Picture is frozen",
                "The source keeps sending, but the camera picture has stayed exactly the same. A frozen camera or a stuck capture looks like this.",
                "Restart the camera or capture source on the encoder.",
            )
        };
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:input:{}", pipeline_id, id),
                Severity::Warning,
                Scope::Pipeline,
                title,
                summary,
                vec![Evidence::measured(format!("unchanged for {} s", ms / 1000))],
                fix,
                generated_at,
            )
        });
    }
    if let Some(ms) = held("silentForMs")
        && ms >= SILENT_ALERT_MS
    {
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:input:audio_silent", pipeline_id),
                Severity::Warning,
                Scope::Pipeline,
                "Sound is silent",
                "The source's first audio track has carried no sound.",
                vec![Evidence::measured(format!("silent for {} s", ms / 1000))],
                "Check the encoder's audio input and mixer, and that the right track is first.",
                generated_at,
            )
        });
    }
    for silent in qa
        .get("silentTracks")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(track), Some(ms)) = (
            silent.get("track").and_then(|v| v.as_u64()),
            silent.get("silentForMs").and_then(|v| v.as_u64()),
        ) else {
            continue;
        };
        if ms < SILENT_ALERT_MS {
            continue;
        }
        alerts.push(Alert {
            pipeline_id: Some(pipeline_id.to_owned()),
            ..Alert::new(
                format!("pipeline:{}:input:audio_silent:track{}", pipeline_id, track + 1),
                Severity::Warning,
                Scope::Pipeline,
                format!("Audio track {} is silent", track + 1),
                format!("The source's audio track {} has carried no sound.", track + 1),
                vec![Evidence::measured(format!("silent for {} s", ms / 1000))],
                "Check that the encoder routes a source to this track, or drop the track if it is not meant to carry sound.",
                generated_at,
            )
        });
    }
}

fn srt_recv_buffer_occupancy(quality: &serde_json::Value) -> Option<(u64, u64, f64)> {
    let buffered = quality.get("srtRecvBufPackets")?.as_u64()?;
    let capacity = quality.get("srtRecvBufCapacityPackets")?.as_u64()?;
    if capacity == 0 {
        return None;
    }
    Some((
        buffered,
        capacity,
        buffered as f64 / capacity as f64 * 100.0,
    ))
}
