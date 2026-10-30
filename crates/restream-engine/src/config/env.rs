//! Environment reads for configuration. Tests overlay them per thread instead
//! of mutating the process environment, so these helpers stay the only place
//! configuration touches `std::env`.

#[cfg(any(test, feature = "test-support"))]
use std::cell::RefCell;
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Per-test environment overrides read by `env_var`: `Some(value)`
    /// sets a variable, `None` removes it. Thread-local, so parallel tests
    /// never see each other's values and nothing mutates the process
    /// environment, which is unsound while other threads run.
    static ENV_OVERLAY: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
}

/// The overlay's answer for `name`, if it overrides it.
#[cfg(any(test, feature = "test-support"))]
fn env_override(name: &str) -> Option<Option<String>> {
    ENV_OVERLAY.with(|overlay| overlay.borrow().get(name).cloned())
}

#[cfg(any(test, feature = "test-support"))]
pub fn with_env_overlay(vars: &[(&str, &str)], removed: &[&str], f: impl FnOnce()) {
    let entries = vars
        .iter()
        .map(|(name, value)| ((*name).to_string(), Some((*value).to_string())))
        .chain(removed.iter().map(|name| ((*name).to_string(), None)));
    let previous = ENV_OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        let previous = overlay.clone();
        overlay.extend(entries);
        previous
    });
    f();
    ENV_OVERLAY.with(|overlay| *overlay.borrow_mut() = previous);
}

#[cfg(any(test, feature = "test-support"))]
pub fn with_env_vars(vars: &[(&str, &str)], f: impl FnOnce()) {
    with_env_overlay(vars, &[], f);
}

pub fn env_var(name: &str) -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(value) = env_override(name) {
        return value;
    }
    std::env::var(name).ok()
}

pub fn env_present(name: &str) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(value) = env_override(name) {
        return value.is_some();
    }
    std::env::var_os(name).is_some()
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub fn env_u32(name: &str, default: u32) -> u32 {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub fn env_usize(name: &str, default: usize) -> usize {
    env_var(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub fn env_bool(name: &str) -> Option<bool> {
    env_var(name).map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}
