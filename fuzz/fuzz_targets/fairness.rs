#![no_main]

use libfuzzer_sys::fuzz_target;
use restream_dataplane::jain_fairness_milli;

fuzz_target!(|visits: Vec<u64>| {
    assert!(jain_fairness_milli(&visits) <= 1_000);
});
