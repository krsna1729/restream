//! Environment reads for configuration. Tests overlay them per thread instead
//! of mutating the process environment, so these helpers stay the only place
//! configuration touches `std::env`.

#[cfg(test)]
use super::configuration_behavior_tests;

pub(super) fn env_var(name: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(value) = configuration_behavior_tests::env_override(name) {
        return value;
    }
    std::env::var(name).ok()
}

pub(super) fn env_present(name: &str) -> bool {
    #[cfg(test)]
    if let Some(value) = configuration_behavior_tests::env_override(name) {
        return value.is_some();
    }
    std::env::var_os(name).is_some()
}

pub(super) fn env_u64(name: &str, default: u64) -> u64 {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(super) fn env_u32(name: &str, default: u32) -> u32 {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(super) fn env_usize(name: &str, default: usize) -> usize {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
