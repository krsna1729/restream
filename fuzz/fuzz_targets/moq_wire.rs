//! Fuzz moq-net's moq-lite and IETF control/object wire decoders.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = moq_net::fuzz::ietf_wire(data);
    let _ = moq_net::fuzz::lite_wire(data);
});
