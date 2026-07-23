use super::super::*;
use crate::media::egress::command::OutputId;
use std::sync::mpsc;

#[test]
fn srt_resolve_worker_sends_resolved_primary_and_backup_hosts() {
    let (sender, mut queue) = srt_resolve_completion_queue(4);

    let handle = spawn_srt_resolve_worker(
        SrtResolveRequest::new(
            OutputId::new("out-a"),
            7,
            vec!["127.0.0.1:9000".to_string(), "127.0.0.2:9001".to_string()],
        ),
        sender,
    );

    assert_eq!(handle.join().unwrap(), Ok(()));
    let mut resolved = Vec::new();
    queue.drain_resolved(&mut resolved);
    assert_eq!(
        resolved,
        vec![SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 7,
            peer_addrs: vec![
                "127.0.0.1:9000".parse().unwrap(),
                "127.0.0.2:9001".parse().unwrap(),
            ],
        }]
    );
}

#[test]
fn srt_resolve_worker_rejects_empty_peer_list() {
    let (sender, _queue) = srt_resolve_completion_queue(4);

    let handle = spawn_srt_resolve_worker(
        SrtResolveRequest::new(OutputId::new("out-a"), 7, Vec::new()),
        sender,
    );

    assert_eq!(
        handle.join().unwrap(),
        Err(SrtResolveWorkerError::EmptyPeerList)
    );
}

#[test]
fn srt_resolve_worker_reports_unresolvable_host_without_completion() {
    let (sender, mut queue) = srt_resolve_completion_queue(4);

    let handle = spawn_srt_resolve_worker(
        SrtResolveRequest::new(
            OutputId::new("out-a"),
            7,
            vec!["256.256.256.256:9000".to_string()],
        ),
        sender,
    );

    assert_eq!(
        handle.join().unwrap(),
        Err(SrtResolveWorkerError::ResolveFailed {
            host: "256.256.256.256:9000".to_string(),
        })
    );
    let mut resolved = Vec::new();
    queue.drain_resolved(&mut resolved);
    assert!(resolved.is_empty());
}

#[test]
fn srt_resolve_worker_reports_full_completion_queue_without_blocking() {
    let (sender, _receiver) = mpsc::sync_channel(0);

    let handle = spawn_srt_resolve_worker(
        SrtResolveRequest::new(
            OutputId::new("out-a"),
            7,
            vec!["127.0.0.1:9000".to_string()],
        ),
        sender,
    );

    assert_eq!(
        handle.join().unwrap(),
        Err(SrtResolveWorkerError::CompletionQueueFull)
    );
}
