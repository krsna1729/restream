use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MediaMtxPathHealth {
    pub(crate) expected_paths: usize,
    pub(crate) ready_paths: usize,
    pub(crate) reader_count: usize,
    pub(crate) bytes_received_before: u64,
    pub(crate) bytes_received_after: u64,
    pub(crate) bytes_received_delta: u64,
    pub(crate) sample_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MediaMtxPathStats {
    ready: bool,
    bytes_received: u64,
    readers: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MediaMtxPathSnapshot {
    paths: HashMap<String, MediaMtxPathStats>,
}

struct MediaMtxPathPage {
    snapshot: MediaMtxPathSnapshot,
    page_count: Option<usize>,
    item_count: Option<usize>,
}

#[cfg(test)]
fn parse_mediamtx_path_snapshot(value: &Value) -> MediaMtxPathSnapshot {
    parse_mediamtx_path_page(value).snapshot
}

fn parse_mediamtx_path_page(value: &Value) -> MediaMtxPathPage {
    let mut snapshot = MediaMtxPathSnapshot::default();
    for item in value["items"].as_array().into_iter().flatten() {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        let readers = item["readers"]
            .as_array()
            .map(|readers| readers.len())
            .or_else(|| item["readers"].as_u64().map(|readers| readers as usize))
            .unwrap_or(0);
        snapshot.paths.insert(
            name.to_string(),
            MediaMtxPathStats {
                ready: item["ready"].as_bool().unwrap_or(false),
                bytes_received: item["bytesReceived"].as_u64().unwrap_or(0),
                readers,
            },
        );
    }
    MediaMtxPathPage {
        snapshot,
        page_count: value["pageCount"].as_u64().map(|count| count as usize),
        item_count: value["itemCount"].as_u64().map(|count| count as usize),
    }
}

async fn fetch_mediamtx_path_snapshot(mtx_api: u16) -> Result<MediaMtxPathSnapshot, String> {
    const ITEMS_PER_PAGE: usize = 100;
    const MAX_PAGES: usize = 10_000;

    let client = reqwest::Client::new();
    let mut combined = MediaMtxPathSnapshot::default();
    let mut page = 0usize;
    let mut expected_item_count = None;

    loop {
        let url = format!(
            "http://127.0.0.1:{mtx_api}/v3/paths/list?page={page}&itemsPerPage={ITEMS_PER_PAGE}"
        );
        let body = client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("failed to query MediaMTX paths API: {error}"))?
            .error_for_status()
            .map_err(|error| format!("MediaMTX paths API returned an error: {error}"))?
            .text()
            .await
            .map_err(|error| format!("failed to read MediaMTX paths API response: {error}"))?;
        let value = serde_json::from_str::<Value>(&body)
            .map_err(|error| format!("failed to parse MediaMTX paths API response: {error}"))?;
        let parsed = parse_mediamtx_path_page(&value);
        expected_item_count = expected_item_count.or(parsed.item_count);
        let page_items = parsed.snapshot.paths.len();
        combined.paths.extend(parsed.snapshot.paths);

        if let Some(page_count) = parsed.page_count {
            if page + 1 >= page_count {
                break;
            }
        } else if page_items < ITEMS_PER_PAGE {
            break;
        }

        page += 1;
        if page >= MAX_PAGES {
            return Err(format!(
                "MediaMTX paths API pagination exceeded {MAX_PAGES} pages"
            ));
        }
    }

    if let Some(item_count) = expected_item_count
        && combined.paths.len() < item_count
    {
        return Err(format!(
            "MediaMTX paths API returned {} unique paths across pages, expected {item_count}",
            combined.paths.len()
        ));
    }

    Ok(combined)
}

fn summarize_paths(paths: &[String]) -> String {
    let mut sample = paths.iter().take(5).cloned().collect::<Vec<_>>();
    if paths.len() > sample.len() {
        sample.push(format!("... +{} more", paths.len() - sample.len()));
    }
    sample.join(", ")
}

fn evaluate_mediamtx_path_health(
    expected_paths: &[String],
    before: &MediaMtxPathSnapshot,
    after: &MediaMtxPathSnapshot,
    sample_secs: u64,
) -> Result<MediaMtxPathHealth, String> {
    let mut missing_before = Vec::new();
    let mut missing_after = Vec::new();
    let mut not_ready = Vec::new();
    let mut stalled = Vec::new();
    let mut ready_paths = 0usize;
    let mut reader_count = 0usize;
    let mut bytes_received_before = 0u64;
    let mut bytes_received_after = 0u64;

    for path in expected_paths {
        let Some(before_stats) = before.paths.get(path) else {
            missing_before.push(path.clone());
            continue;
        };
        let Some(after_stats) = after.paths.get(path) else {
            missing_after.push(path.clone());
            continue;
        };
        if after_stats.ready {
            ready_paths += 1;
        } else {
            not_ready.push(path.clone());
        }
        if after_stats.bytes_received <= before_stats.bytes_received {
            stalled.push(format!(
                "{path} ({} -> {})",
                before_stats.bytes_received, after_stats.bytes_received
            ));
        }
        reader_count += after_stats.readers;
        bytes_received_before = bytes_received_before.saturating_add(before_stats.bytes_received);
        bytes_received_after = bytes_received_after.saturating_add(after_stats.bytes_received);
    }

    if !missing_before.is_empty()
        || !missing_after.is_empty()
        || !not_ready.is_empty()
        || !stalled.is_empty()
    {
        let mut reasons = Vec::new();
        if !missing_before.is_empty() {
            reasons.push(format!(
                "missing before sample: {}",
                summarize_paths(&missing_before)
            ));
        }
        if !missing_after.is_empty() {
            reasons.push(format!(
                "missing after sample: {}",
                summarize_paths(&missing_after)
            ));
        }
        if !not_ready.is_empty() {
            reasons.push(format!("not ready: {}", summarize_paths(&not_ready)));
        }
        if !stalled.is_empty() {
            reasons.push(format!(
                "bytesReceived stalled: {}",
                summarize_paths(&stalled)
            ));
        }
        return Err(format!(
            "MediaMTX path health failed for {} expected paths (observed before={} after={}): {}",
            expected_paths.len(),
            before.paths.len(),
            after.paths.len(),
            reasons.join("; ")
        ));
    }

    Ok(MediaMtxPathHealth {
        expected_paths: expected_paths.len(),
        ready_paths,
        reader_count,
        bytes_received_before,
        bytes_received_after,
        bytes_received_delta: bytes_received_after.saturating_sub(bytes_received_before),
        sample_secs,
    })
}

pub(crate) async fn verify_mediamtx_path_health(
    mtx_api: u16,
    expected_paths: &[String],
    sample_secs: u64,
    timeout: Duration,
) -> Result<MediaMtxPathHealth, String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;

    loop {
        let before = fetch_mediamtx_path_snapshot(mtx_api).await?;
        tokio::time::sleep(Duration::from_secs(sample_secs)).await;
        let after = fetch_mediamtx_path_snapshot(mtx_api).await?;
        match evaluate_mediamtx_path_health(expected_paths, &before, &after, sample_secs) {
            Ok(health) => return Ok(health),
            Err(error) if Instant::now() < deadline => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(error) => {
                let first_error = last_error
                    .map(|previous| format!(" first failure: {previous};"))
                    .unwrap_or_default();
                return Err(format!(
                    "{error};{first_error} timed out after {}s",
                    timeout.as_secs()
                ));
            }
        }
    }
}

pub(crate) fn mediamtx_path_health_json(
    scenario: &str,
    label: &str,
    health: &MediaMtxPathHealth,
) -> Value {
    json!({
        "kind": "mediamtxPathHealth",
        "scenario": scenario,
        "label": label,
        "expectedPaths": health.expected_paths,
        "readyPaths": health.ready_paths,
        "readerCount": health.reader_count,
        "bytesReceivedBefore": health.bytes_received_before,
        "bytesReceivedAfter": health.bytes_received_after,
        "bytesReceivedDelta": health.bytes_received_delta,
        "sampleSecs": health.sample_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mediamtx_paths_and_accepts_growing_ready_outputs() {
        let before = parse_mediamtx_path_snapshot(&json!({
            "itemCount": 2,
            "pageCount": 1,
            "items": [
                {"name": "live/a", "ready": true, "bytesReceived": 100, "readers": []},
                {"name": "live/b", "ready": true, "bytesReceived": 200, "readers": [{"id": "r1"}]}
            ]
        }));
        let after = parse_mediamtx_path_snapshot(&json!({
            "items": [
                {"name": "live/a", "ready": true, "bytesReceived": 140, "readers": []},
                {"name": "live/b", "ready": true, "bytesReceived": 260, "readers": [{"id": "r1"}, {"id": "r2"}]}
            ]
        }));

        let health =
            evaluate_mediamtx_path_health(&["live/a".into(), "live/b".into()], &before, &after, 3)
                .expect("ready paths with byte growth should pass");

        assert_eq!(health.expected_paths, 2);
        assert_eq!(health.ready_paths, 2);
        assert_eq!(health.reader_count, 2);
        assert_eq!(health.bytes_received_before, 300);
        assert_eq!(health.bytes_received_after, 400);
        assert_eq!(health.bytes_received_delta, 100);
    }

    #[test]
    fn parses_mediamtx_path_page_metadata_for_pagination() {
        let page = parse_mediamtx_path_page(&json!({
            "itemCount": 120,
            "pageCount": 2,
            "items": [{"name": "live/a", "ready": true, "bytesReceived": 100}]
        }));

        assert_eq!(page.item_count, Some(120));
        assert_eq!(page.page_count, Some(2));
        assert!(page.snapshot.paths.contains_key("live/a"));
    }

    #[test]
    fn rejects_mediamtx_path_without_byte_growth() {
        let before = parse_mediamtx_path_snapshot(&json!({
            "items": [{"name": "live/a", "ready": true, "bytesReceived": 100}]
        }));
        let after = parse_mediamtx_path_snapshot(&json!({
            "items": [{"name": "live/a", "ready": true, "bytesReceived": 100}]
        }));

        let error = evaluate_mediamtx_path_health(&["live/a".into()], &before, &after, 3)
            .expect_err("stalled bytesReceived must fail the checkpoint");

        assert!(error.contains("bytesReceived stalled"));
        assert!(error.contains("live/a (100 -> 100)"));
    }

    #[test]
    fn rejects_missing_or_unready_mediamtx_paths() {
        let before = parse_mediamtx_path_snapshot(&json!({
            "items": [
                {"name": "live/a", "ready": true, "bytesReceived": 100},
                {"name": "live/b", "ready": true, "bytesReceived": 100}
            ]
        }));
        let after = parse_mediamtx_path_snapshot(&json!({
            "items": [
                {"name": "live/a", "ready": false, "bytesReceived": 200}
            ]
        }));

        let error =
            evaluate_mediamtx_path_health(&["live/a".into(), "live/b".into()], &before, &after, 3)
                .expect_err("missing and unready paths must fail the checkpoint");

        assert!(error.contains("missing after sample: live/b"));
        assert!(error.contains("not ready: live/a"));
    }
}
