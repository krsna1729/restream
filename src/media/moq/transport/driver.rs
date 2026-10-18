//! Runs a moq-net driver on the Compio runtime: `moq_net::time::run` with
//! Compio timers instead of Tokio's.

use std::pin::Pin;
use std::task::Poll;
use std::time::Instant;

/// The armed Compio sleep and the deadline it was armed for.
type Armed = (Instant, Pin<Box<dyn Future<Output = ()>>>);

/// Polls `driver` until it finishes, sleeping (Compio timer) until each
/// deadline it asks for and waking on its own activity in between. Resolves
/// with the driver's terminal error, `moq_net::Error::Closed` for a clean
/// finish. `now` never goes backwards (moq-net's clock asserts on that).
pub(crate) async fn run_driver<D: moq_net::time::Driver>(mut driver: D) -> moq_net::Error {
    // The sleep is replaced only when the deadline changes.
    let mut sleep: Option<Armed> = None;
    let mut last = Instant::now();
    kio::wait(|waiter| {
        loop {
            let now = Instant::now().max(last);
            last = now;
            let at = match driver.poll(now, waiter) {
                Ok(Some(at)) => at,
                Ok(None) => {
                    sleep = None;
                    return Poll::Pending;
                }
                Err(error) => return Poll::Ready(error),
            };
            let (_, timer) = match &mut sleep {
                Some(armed) if armed.0 == at => armed,
                slot => slot.insert((at, Box::pin(compio::time::sleep_until(at)))),
            };
            if waiter.poll_future(timer.as_mut()).is_pending() {
                return Poll::Pending;
            }
            // Fired: a fired sleep must not be polled again.
            sleep = None;
        }
    })
    .await
}
