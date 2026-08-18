use std::sync::{Mutex, MutexGuard};

static EXPECTED_PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

pub(crate) struct ExpectedPanicSilencer {
    _lock: MutexGuard<'static, ()>,
    previous_hook: Option<PanicHook>,
}

pub(crate) fn silence_expected_panics() -> ExpectedPanicSilencer {
    let lock = EXPECTED_PANIC_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    ExpectedPanicSilencer {
        _lock: lock,
        previous_hook: Some(previous_hook),
    }
}

impl Drop for ExpectedPanicSilencer {
    fn drop(&mut self) {
        if let Some(previous_hook) = self.previous_hook.take() {
            std::panic::set_hook(previous_hook);
        }
    }
}
