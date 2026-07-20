#[path = "root_tests/harness_contracts.rs"]
mod harness_contracts;
#[path = "root_tests/matrix_contracts.rs"]
mod matrix_contracts;
#[path = "root_tests/media_validation.rs"]
mod media_validation;
#[path = "root_tests/mixed_execution.rs"]
mod mixed_execution;
#[path = "root_tests/output_contracts.rs"]
mod output_contracts;

fn mixed_runner_matrix_source() -> String {
    [
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_matrix_runner.rs"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_matrix_execution.rs"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_matrix_progress.rs"
        )),
    ]
    .join("\n")
}
