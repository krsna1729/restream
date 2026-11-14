//! Real-process sibling isolation under environmental and entity faults.
use std::process::Stdio;

use futures_util::stream::{FuturesUnordered, StreamExt};
use sqlx::{Connection, SqliteConnection};
use tokio::io::AsyncWriteExt;

use super::super::*;
use super::sibling_sampler::{Victim, watch};
use super::sibling_wire;

const MODE: &str = "fault.sibling-isolation";

/// The test target owns FFmpeg descendants too: killing only its leader on
/// timeout would bypass the target's Rust cleanup and orphan those children.
struct BoundaryProcessGroup(u32);

impl Drop for BoundaryProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the target was created as leader of this owned process group;
        // negative PID signalling addresses only that group's descendants.
        unsafe {
            libc::kill(-(self.0 as libc::pid_t), libc::SIGKILL);
        }
    }
}

pub(crate) async fn fault_sibling_isolation() -> Result<Value, String> {
    let dir = artifact_path(MODE);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let test_binary = std::env::var_os("RESTREAM_FAULT_TEST_BIN").ok_or(
        "prebuilt live fault target missing; run cargo xtask harness fault.sibling-isolation",
    )?;
    let child = Command::new(test_binary)
        .args(["live_sibling_fault_gate_", "--test-threads=1"])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let group = BoundaryProcessGroup(child.id().ok_or("live fault target PID missing")?);
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| "live fault target exceeded 120 s")?
        .map_err(|error| error.to_string())?;
    drop(group);
    std::fs::write(
        dir.join("live-boundaries.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout).contains("test result: ok. 2 passed; 0 failed;")
    {
        return Err(format!(
            "live fault target failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let ports = TestPorts::from_env();
    let db = dir.join("data.sqlite");
    let mut child = start_restream_child_with_stdout(
        &default_restream_bin(),
        &ports,
        &db,
        (&dir.join("restream.log"), Some(Stdio::piped())),
        true,
        None,
        &[],
    )
    .await?;
    // Keep the read end open, never read it. Closing it would test EPIPE instead.
    let _unread = child.stdout.take().ok_or("stdout pipe missing")?;
    let result = run(&ports, &db).await;
    stop_child(&mut child).await;
    let tests = result?;
    super::finish_fault_mode(
        &dir,
        MODE,
        true,
        json!({"mode": MODE, "passed": true, "nonShippingLiveBoundaryTests": 2, "tests": tests}),
    )
}

async fn run(ports: &TestPorts, db: &Path) -> Result<Vec<Value>, String> {
    let api = login_api(ports).await?;
    let fixture = checked_h264_fixture()?;
    let base = harness_port_defaults().ffmpeg_srt_sink_base;
    let mut publishers = Vec::new();
    let mut sinks = Vec::new();
    let mut victims = Vec::new();
    for (index, name) in ["isolation-a", "isolation-b"].into_iter().enumerate() {
        let pipeline = create_pipeline_with_stream_key(&api, name, name).await?;
        let standby = create_backup_input(&api, &pipeline).await?;
        for key in [name, &standby.stream_key] {
            publishers.push(
                spawn_publisher(
                    &fixture,
                    &format!("rtmp://127.0.0.1:{}/live/{key}", ports.rtmp),
                    "flv",
                    false,
                )
                .await?,
            );
        }
        wait_for_api_input_live(&api, &pipeline, Duration::from_secs(20)).await?;
        wait_for_input_state(
            &api,
            &pipeline,
            &standby.id,
            "standby",
            Duration::from_secs(20),
        )
        .await?;
        let inputs = api
            .get_json(&format!("/api/v1/pipelines/{pipeline}/inputs"))
            .await?;
        let selected = inputs["selectedInputId"]
            .as_str()
            .ok_or("selected input missing")?
            .to_string();
        let port = base
            .checked_add(3200 + index as u16)
            .ok_or("sink port overflow")?;
        let sink = HarnessSrtSinkPool::start(&[port], 0)?;
        let output = create_output(
            &api,
            &pipeline,
            "healthy",
            &harness_srt_output_url(port, name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        start_output(&api, &pipeline, &output).await?;
        wait_for_outputs_progress(
            &api,
            &pipeline,
            std::slice::from_ref(&output),
            Duration::from_secs(20),
        )
        .await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while sink.snapshot().discarded_bytes < 10_000 {
            if Instant::now() >= deadline {
                return Err(format!(
                    "victim {name} never delivered to its real consumer"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        victims.push(Victim {
            pipeline,
            output,
            selected,
            standby: standby.id,
            sink: sink.counters(),
        });
        sinks.push(sink);
    }
    let a = &victims[0].pipeline;
    let doomed = create_backup_input(&api, a).await?;
    let mut tests = Vec::new();
    tests.push(
        watch(&api, &victims, async {
            super::destination_stall::rtmp_destination_stalls(
                &api,
                a,
                base.checked_add(3202).ok_or("stall port overflow")?,
            )
            .await
        })
        .await?,
    );
    tests.push(
        watch(&api, &victims, async {
            sibling_wire::malformed(ports.rtmp, &doomed.stream_key).await?;
            Ok(json!({"case": "malformed-standby-input", "retiredWithinMs": 5000}))
        })
        .await?,
    );
    tests.push(
        watch(&api, &victims, async {
            let mut stalled = sibling_wire::peer(ports.rtmp, &doomed.stream_key, true).await?;
            tokio::time::sleep(Duration::from_secs(8)).await;
            let mut pending = [0; 4096];
            loop {
                match stalled.try_read(&mut pending) {
                    Ok(0) => return Err("silent publisher disconnected before its 8 s hold".into()),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        return Err(format!("silent publisher lost its connection: {error}"));
                    }
                }
            }
            stalled.shutdown().await.map_err(|e| e.to_string())?;
            Ok(json!({"case": "publisher-stall", "heldMs": 8000}))
        })
        .await?,
    );
    tests.push(
        watch(&api, &victims, async {
            let queued = sibling_wire::stopped_reader(ports.rtmp, "isolation-a").await?;
            Ok(json!({"case": "stopped-reader", "heldMs": 20000, "unreadSocketBytes": queued}))
        })
        .await?,
    );
    tests.push(watch(&api, &victims, async {
        let mut connection = SqliteConnection::connect(&format!("sqlite:{}", db.display())).await.map_err(|e| e.to_string())?;
        let held = connection.begin_with("BEGIN IMMEDIATE").await.map_err(|e| e.to_string())?;
        // A real write must remain pending, rather than calling a read-only
        // endpoint and claiming that SQLite was stalled.
        let update = api.post_json("/api/v1/pipelines", json!({"name": "after-lock", "streamKey": "after-lock"}));
        tokio::pin!(update);
        tokio::select! {
            result = &mut update => return Err(format!("write did not stall behind writer lock: {result:?}")),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        // Admission uses the immutable catalog snapshot while the real writer
        // and the API's persistence operation are held.
        let mut publisher = spawn_publisher(&fixture, &harness_srt_ffmpeg_url(ports.srt, &doomed.stream_key, HarnessSrtMode::Publish, None), "mpegts", false).await?;
        wait_for_input_state(&api, a, &doomed.id, "standby", Duration::from_secs(5)).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        stop_srt_publisher(&mut publisher).await?;
        held.rollback().await.map_err(|e| e.to_string())?;
        let converged = tokio::time::timeout(Duration::from_secs(10), update).await.map_err(|_| "SQLite write did not converge after release")??;
        if converged["pipeline"]["id"].as_str().is_none() {
            return Err(format!("SQLite write did not return its persisted pipeline: {converged}"));
        }
        Ok(json!({"case": "sqlite-writer-stall", "writeConverged": !converged["pipeline"]["id"].is_null(), "snapshotAdmission": true}))
    }).await?);
    tests.push(watch(&api, &victims, async {
        let before = console_dropped(&api).await?;
        let mut connections = 0;
        // Each accepted RTMP play/close emits only lifecycle events. No new
        // logging flag, queue override, packet logs or synthetic production hook.
        while console_dropped(&api).await? <= before {
            let mut peers = FuturesUnordered::new();
            for _ in 0..16 {
                peers.push(sibling_wire::peer(ports.rtmp, "isolation-a", false));
            }
            while let Some(peer) = peers.next().await {
                drop(peer?);
                connections += 1;
            }
            if connections > 12000 { return Err("lifecycle load did not fill stdout pipe and logger queue".into()); }
        }
        let dropped_at_connect = console_dropped(&api).await?;
        let started = Instant::now();
        let mut publisher = spawn_publisher(&fixture, &harness_srt_ffmpeg_url(ports.srt, &doomed.stream_key, HarnessSrtMode::Publish, None), "mpegts", false).await?;
        wait_for_input_state(&api, a, &doomed.id, "standby", Duration::from_secs(5)).await?;
        let connect_ms = started.elapsed().as_millis();
        if connect_ms > 5000 { return Err(format!("SRT connect took {connect_ms} ms with stdout stalled")); }
        let disconnected = Instant::now();
        stop_srt_publisher(&mut publisher).await?;
        loop {
            let inputs = api.get_json(&format!("/api/v1/pipelines/{a}/inputs")).await?;
            if inputs["inputs"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == doomed.id && row["runtime"]["connected"] == false)) { break; }
            if disconnected.elapsed() > Duration::from_secs(5) { return Err("SRT disconnect exceeded 5 s with stdout stalled".into()); }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if disconnected.elapsed() > Duration::from_secs(5) {
            return Err("SRT disconnect exceeded 5 s with stdout stalled".into());
        }
        let after = console_dropped(&api).await?;
        if after <= dropped_at_connect { return Err("console drops did not rise through SRT lifecycle".into()); }
        Ok(json!({"case": "stdout-never-drained", "lifecycleConnections": connections, "consoleDroppedBefore": before, "consoleDroppedAtConnect": dropped_at_connect, "consoleDroppedAfter": after, "srtConnectMs": connect_ms, "srtDisconnectMs": disconnected.elapsed().as_millis()}))
    }).await?);
    for publisher in &mut publishers {
        stop_child(publisher).await;
    }
    for sink in sinks {
        sink.stop();
    }
    Ok(tests)
}

async fn console_dropped(api: &RampApi) -> Result<u64, String> {
    api.get_json("/api/v1/engine/health").await?["logging"]["consoleDropped"]
        .as_u64()
        .ok_or_else(|| "logging.consoleDropped missing".into())
}

async fn stop_srt_publisher(child: &mut Child) -> Result<(), String> {
    let pid = child.id().ok_or("SRT publisher exited before disconnect")?;
    // SAFETY: pid belongs to the live child held here. SIGINT lets FFmpeg close
    // its SRT socket normally; SIGKILL would instead test the peer-idle timer.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .map_err(|_| "SRT publisher did not exit within 3 s after SIGINT")?
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn boundary_process_group_drop_stops_a_live_descendant() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 60 & echo $!; wait"])
            .process_group(0)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("owned process group");
        let group = BoundaryProcessGroup(child.id().expect("group leader PID"));
        let mut stdout = BufReader::new(child.stdout.take().expect("descendant PID pipe")).lines();
        let descendant: u32 = tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
            .await
            .expect("descendant starts on time")
            .expect("PID pipe readable")
            .expect("descendant PID")
            .parse()
            .expect("numeric PID");
        drop(group);
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("group leader exits")
            .expect("leader reaped");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let running = std::fs::read_to_string(format!("/proc/{descendant}/stat"))
                .is_ok_and(|stat| !stat.contains(") Z "));
            if !running {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the target's descendant survived its group"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
