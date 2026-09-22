//! Unit tests for the `egress-duty` mode's pure logic: shard-thread
//! selection, CPU derivation, receiver-TSV parsing, mask validation and the
//! product-side burst bound.

use std::collections::BTreeSet;

use super::*;

fn comm(tid: u32, name: &str) -> ThreadComm {
    ThreadComm {
        tid,
        comm: name.to_string(),
    }
}

#[test]
fn egress_shard_threads_are_selected_from_a_real_thread_list() {
    let threads = vec![
        comm(101, "restream"),
        comm(102, "tokio-runtime-w"),
        comm(103, "ffmpeg"),
        comm(104, "egress-shard-0"),
        comm(105, "egress-shard-1"),
        comm(106, "srt-ingest"),
        // Not a shard name: the prefix must start the comm.
        comm(107, "x-egress-shard-3"),
        // Digits must be digits: a handshake thread that happens to share
        // the visual prefix is not a shard.
        comm(108, "egress-shard-x"),
        comm(109, "egress-shard-"),
    ];
    let selected = select_egress_shard_threads(&threads);
    assert_eq!(
        selected,
        vec![
            EgressShardThread {
                tid: 104,
                index: 0,
                comm_truncated: false,
            },
            EgressShardThread {
                tid: 105,
                index: 1,
                comm_truncated: false,
            },
        ]
    );
}

#[test]
fn egress_shard_comm_truncation_is_reported_not_silently_trusted() {
    // 14 bytes: `egress-shard-0` — unambiguous.
    assert_eq!("egress-shard-0".len(), 14);
    assert_eq!(parse_egress_shard_comm("egress-shard-0"), Some((0, false)));
    assert_eq!(parse_egress_shard_comm("egress-shard-1"), Some((1, false)));
    // 15 bytes is exactly the kernel's limit, so these digits may be a
    // prefix of a longer index rather than the index itself.
    assert_eq!("egress-shard-12".len(), COMM_MAX_BYTES);
    assert_eq!(parse_egress_shard_comm("egress-shard-12"), Some((12, true)));
    // The kernel renders `egress-shard-120` as exactly that same comm, so
    // a selection that ignored the limit would silently pin the wrong
    // thread's neighbour as shard 12.
    assert_eq!(
        "egress-shard-120"
            .chars()
            .take(COMM_MAX_BYTES)
            .collect::<String>(),
        "egress-shard-12"
    );
    let selected = select_egress_shard_threads(&[comm(201, "egress-shard-120")]);
    assert_eq!(selected.len(), 1);
    assert!(selected[0].comm_truncated);
    // /proc's trailing newline is not part of the name.
    assert_eq!(
        parse_egress_shard_comm("egress-shard-0\n"),
        Some((0, false))
    );
}

#[test]
fn cpu_derivations_cover_zero_and_reset_denominators() {
    let ticks = 100;
    assert_eq!(
        cpu_secs_between(
            CpuTicks {
                user: 100,
                system: 50
            },
            CpuTicks {
                user: 250,
                system: 150
            },
            ticks,
        ),
        Some(2.5)
    );
    // A counter that went backwards is a reset (or a recycled pid), not a
    // negative delta.
    assert_eq!(
        cpu_secs_between(
            CpuTicks {
                user: 250,
                system: 150
            },
            CpuTicks {
                user: 100,
                system: 50
            },
            ticks,
        ),
        None
    );
    assert_eq!(
        cpu_secs_between(CpuTicks::default(), CpuTicks::default(), 0),
        None
    );

    // The per-DATA and per-wire ratios.
    assert_eq!(micros_per_unit(2.5, 5000), Some(500.0));
    assert_eq!(micros_per_unit(2.5, 2500), Some(1000.0));
    // An empty window has no ratio: `None`, never 0.
    assert_eq!(micros_per_unit(2.5, 0), None);
    assert_eq!(micros_per_unit(0.0, 0), None);
    // Zero CPU over a non-empty window IS a number: 0.0 us per datagram.
    assert_eq!(micros_per_unit(0.0, 1000), Some(0.0));

    // Deltas read through the counter tree inherit the same discipline.
    let before = json!({"txClass": {"dataFirst": 10, "dataRetransmit": 0}});
    let after = json!({"txClass": {"dataFirst": 40, "dataRetransmit": 2}});
    let delta = counter_deltas(&before, &after);
    assert_eq!(counter_at(&delta, &["txClass", "dataFirst"]), Some(30));
    assert_eq!(counter_at(&delta, &["txClass", "dataRetransmit"]), Some(2));
    // Absent on one side ⇒ null, not a fabricated zero.
    let missing = counter_deltas(&json!({"a": Value::Null}), &json!({"a": 5}));
    assert_eq!(counter_at(&missing, &["a"]), None);
    assert_eq!(missing["a"], Value::Null);
}

#[test]
fn proc_stat_parses_ticks_around_a_comm_with_spaces() {
    // Field 2 is a parenthesised comm that can contain spaces and parens,
    // so a whitespace split shifts utime/stime. After the last `)` the
    // tokens start at `state` (field 3); utime is field 14 and stime 15.
    let stat = "42 (egress shard (0)) S 1 2 3 4 5 6 7 8 9 10 111 222";
    assert_eq!(
        parse_proc_stat_cpu(stat).unwrap(),
        CpuTicks {
            user: 111,
            system: 222
        }
    );
}

#[test]
fn receiver_tsv_maps_by_header_and_rejects_a_width_mismatch() {
    let header = RECEIVER_REQUIRED_COLUMNS.join("\t");
    let row = "4800\t5000\t0\t0\t11\t30.004\t120.5\t40.5\t0\t0\t0\t0\t0\t0";
    let text = format!("{header}\n{row}\n");
    let parsed = parse_receiver_tsv(&text).expect("valid single-row TSV");
    assert_eq!(parsed.number("pkt_sent"), Some(4800));
    assert_eq!(parsed.number("core_total"), Some(5000));
    assert_eq!(parsed.number("established"), Some(11));
    assert_eq!(parsed.number("sec_a"), Some(0));
    assert_eq!(parsed.float("elapsed_s"), Some(30.004));

    // One column short: every following value would shift left, so this
    // must not be read as a shorter row.
    let short_row = "4800\t5000\t0\t0\t11\t30.004\t120.5\t40.5\t0\t0\t0\t0\t0";
    let error = parse_receiver_tsv(&format!("{header}\n{short_row}\n"))
        .expect_err("width mismatch must be rejected");
    assert!(error.contains("13 columns"), "{error}");
    assert!(error.contains("14"), "{error}");

    // A missing required column is rejected rather than read as zero.
    let header_missing = RECEIVER_REQUIRED_COLUMNS[1..].join("\t");
    let error = parse_receiver_tsv(&format!("{header_missing}\n{row}\n"))
        .expect_err("missing required column must be rejected");
    assert!(error.contains("pkt_sent"), "{error}");

    // The pinned receiver appends exactly one row per process; a stale
    // second row means this file is not this run's result.
    let error = parse_receiver_tsv(&format!("{header}\n{row}\n{row}\n"))
        .expect_err("a second data row must be rejected");
    assert!(error.contains("one row per process"), "{error}");
}

#[test]
fn effective_visit_max_bytes_is_read_from_the_products_own_config_event() {
    let line = r#"2026-09-22T18:36:00Z  INFO restream::infrastructure::bootstrap: effective startup configuration event_class="lifecycle" event_type="restream.config.effective" http_port=1 summary={"egressFabric":{"commandBatchBudget":32,"maxPendingBytes":262144,"visitMaxBytes":262144,"visitMaxUs":2000}}"#;
    assert_eq!(parse_visit_max_bytes(line), Some(262_144));
    // The bound has to cover the queue the receiver actually built:
    // 262144 / 1316 = 199 datagrams per visit.
    assert_eq!(
        parse_visit_max_bytes(line).map(|bytes| bytes / u64::from(HARNESS_SRT_PACKET_SIZE)),
        Some(199)
    );
    // Absent / malformed reads as `null`, never a fabricated number.
    assert_eq!(parse_visit_max_bytes("no config line here"), None);
    assert_eq!(parse_visit_max_bytes(r#"{"visitMaxBytes":"nope"}"#), None);
}

#[test]
fn cpu_masks_validate_as_sets() {
    assert_eq!(parse_cpu_mask("0").unwrap(), BTreeSet::from([0]));
    assert_eq!(parse_cpu_mask("2-5").unwrap(), BTreeSet::from([2, 3, 4, 5]));
    assert_eq!(parse_cpu_mask("0,1").unwrap(), BTreeSet::from([0, 1]));
    assert_eq!(parse_cpu_mask("0-1,4").unwrap(), BTreeSet::from([0, 1, 4]));
    assert!(parse_cpu_mask("5-2").is_err());
    assert!(parse_cpu_mask("").is_err());
    assert!(parse_cpu_mask("x").is_err());
    // A range is not a single CPU.
    assert!(single_cpu(&parse_cpu_mask("2-5").unwrap(), "KNOB").is_err());
    assert_eq!(
        single_cpu(&parse_cpu_mask("3").unwrap(), "KNOB").unwrap(),
        3
    );

    let shard = 0;
    assert!(validate_cpu_masks(shard, &BTreeSet::from([1]), &BTreeSet::from([2, 3]), None).is_ok());
    assert!(validate_cpu_masks(shard, &BTreeSet::from([0]), &BTreeSet::from([2]), None).is_err());
    assert!(validate_cpu_masks(shard, &BTreeSet::from([1]), &BTreeSet::from([0]), None).is_err());
    assert!(
        validate_cpu_masks(
            shard,
            &BTreeSet::from([1]),
            &BTreeSet::from([2]),
            Some(BTreeSet::from([0, 1])),
        )
        .is_err()
    );
}

#[test]
fn canonical_srt_ingest_row_is_still_the_sweep_h264_srt_row() {
    // The mode hard-codes the sweep's canonical SRT-ingest shape; if the
    // DSL row ever changes, the measurement would silently describe a
    // different source.
    let rows: Vec<Value> = serde_json::from_str(include_str!("../sweep_configs.json")).unwrap();
    let row = rows
        .iter()
        .find(|row| row["name"] == "h264-srt")
        .expect("sweep_configs.json must keep an h264-srt row");
    assert_eq!(row["ingestProto"], "srt");
    assert_eq!(row["videoCodec"], "h264");
    assert_eq!(row["multiAudio"], false);
}
