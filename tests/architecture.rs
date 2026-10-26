//! Architecture and layering rules, one test binary: each module guards one
//! boundary (layering, transport ownership, service wiring, runtime and
//! media contracts, domain dependencies) by reading the source it governs.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

#[path = "architecture/compliance.rs"]
mod compliance;
#[path = "architecture/domain_layering.rs"]
mod domain_layering;
#[path = "architecture/media_core.rs"]
mod media_core;
#[path = "architecture/runtime_contract.rs"]
mod runtime_contract;
#[path = "architecture/service_wiring.rs"]
mod service_wiring;
#[path = "architecture/transport.rs"]
mod transport;

/// Visit every `.rs` file under `directory`, recursively.
fn collect_rust_sources(
    directory: &std::path::Path,
    inspect: &mut impl FnMut(&std::path::Path, &str),
) {
    for entry in std::fs::read_dir(directory).expect("Rust source directory should be readable") {
        let path = entry.expect("Rust source entry should be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, inspect);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            let source = std::fs::read_to_string(&path).expect("Rust source should be UTF-8");
            inspect(&path, &source);
        }
    }
}
