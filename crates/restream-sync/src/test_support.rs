use std::{cell::Cell, panic, sync::Once};

thread_local! {
    static SUPPRESS_EXPECTED_PANIC_HOOK: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn with_expected_panic_suppressed<R>(operation: impl FnOnce() -> R) -> R {
    static INSTALL_PANIC_HOOK: Once = Once::new();

    struct RestoreSuppression(bool);
    impl Drop for RestoreSuppression {
        fn drop(&mut self) {
            SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.set(self.0));
        }
    }

    INSTALL_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !SUPPRESS_EXPECTED_PANIC_HOOK.with(Cell::get) {
                previous(info);
            }
        }));
    });
    let previous = SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.replace(true));
    let _restore = RestoreSuppression(previous);
    operation()
}
