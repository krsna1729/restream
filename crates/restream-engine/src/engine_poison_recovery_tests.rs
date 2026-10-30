use super::*;

// Regression: active_egress_diag_snapshots reads `phase`/`target_addr`/
// `last_error` via `unwrap_or_else(|e| e.into_inner())`, which is supposed to
// recover a poisoned std::sync::Mutex rather than propagate the panic into
// the diagnostics endpoint. That recovery path had no dedicated test.
#[tokio::test]
async fn active_egress_diag_snapshots_recovers_from_poisoned_locks() {
    let engine = MediaEngine::new();
    engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", None)
        .await;

    let (phase, target_addr, last_error) = {
        let egresses = engine.egresses.active.read().await;
        let egress = egresses.get("out-1").expect("registered egress");
        (
            egress.phase.clone(),
            egress.target_addr.clone(),
            egress.last_error.clone(),
        )
    };

    let p = phase.clone();
    let _ = std::thread::spawn(move || {
        crate::test_support::with_expected_panic_suppressed(|| {
            let _guard = p.lock().unwrap();
            panic!("deliberate poison: phase");
        });
    })
    .join();
    assert!(phase.lock().is_err(), "phase mutex should be poisoned");

    let t = target_addr.clone();
    let _ = std::thread::spawn(move || {
        crate::test_support::with_expected_panic_suppressed(|| {
            let mut guard = t.lock().unwrap();
            *guard = Some("poisoned-addr".to_string());
            panic!("deliberate poison: target_addr");
        });
    })
    .join();
    assert!(
        target_addr.lock().is_err(),
        "target_addr mutex should be poisoned"
    );

    let e = last_error.clone();
    let _ = std::thread::spawn(move || {
        crate::test_support::with_expected_panic_suppressed(|| {
            let _guard = e.lock().unwrap();
            panic!("deliberate poison: last_error");
        });
    })
    .join();
    assert!(
        last_error.lock().is_err(),
        "last_error mutex should be poisoned"
    );

    let snapshots = engine.active_egress_diag_snapshots("pipe-1").await;
    assert_eq!(
        snapshots.len(),
        1,
        "poisoned locks must not hide the egress from diagnostics"
    );
    assert_eq!(snapshots[0].target_addr.as_deref(), Some("poisoned-addr"));
}
