#![cfg(feature = "loom")]

use loom::sync::Arc;
use loom::thread;
use restream_dataplane::WakeGate;

#[test]
fn concurrent_notifications_are_coalesced() {
    loom::model(|| {
        let gate = Arc::new(WakeGate::new());
        let left = Arc::clone(&gate);
        let right = Arc::clone(&gate);
        let a = thread::spawn(move || left.notify());
        let b = thread::spawn(move || right.notify());
        let notifications = u8::from(a.join().unwrap()) + u8::from(b.join().unwrap());
        assert_eq!(notifications, 1);
    });
}
