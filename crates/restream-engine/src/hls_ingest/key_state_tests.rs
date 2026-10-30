//! The key state's lifetime: an input whose row is gone releases what its key
//! applied.

use super::*;
use crate::hls_ingest::test_support::{INPUT_ID, test_ingest};

/// `forget_input` clears the applied init. The key itself stays while a caller
/// holds it: eviction waits for the last holder, which in production is the
/// session that applied the init.
#[tokio::test]
async fn forget_input_clears_the_applied_init() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    let key = ingest.key_state(INPUT_ID);
    ingest.seed_applied_init_for_tests(INPUT_ID);
    ingest.forget_input(INPUT_ID);
    assert!(
        restream_sync::lock::lock(&key.init).is_empty(),
        "the applied init is released"
    );
    assert_eq!(
        ingest.keyed_input_ids(),
        std::collections::HashSet::from([INPUT_ID.to_string()]),
        "the key state stays while this test holds it"
    );
}

/// Once nothing holds it, a forgotten key's state is evicted.
#[tokio::test]
async fn an_idle_key_state_is_evicted_when_it_is_forgotten() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    ingest.seed_applied_init_for_tests(INPUT_ID);
    ingest.forget_input(INPUT_ID);
    assert!(
        ingest.keyed_input_ids().is_empty(),
        "an idle key is evicted once nothing holds it"
    );
}

/// Another input's key state is not touched.
#[tokio::test]
async fn forget_input_leaves_another_inputs_key_state_alone() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    ingest.seed_applied_init_for_tests("input-other");
    ingest.forget_input(INPUT_ID);
    assert_eq!(
        ingest.keyed_input_ids(),
        std::collections::HashSet::from(["input-other".to_string()]),
        "only the named input's key state is released"
    );
}
