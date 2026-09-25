//! Aggregate bound on bytes held by RTMP ingest parsers.
//!
//! The media-handoff semaphore only covers messages that finished parsing; a
//! parser holds unprocessed input and a partially assembled message before
//! that. Each connection's own message size is capped by
//! `ServerSessionConfig::max_message_length`; this budget caps the sum across
//! connections. Every connection future runs on the single RTMP Compio owner
//! thread, so the shared total is a plain `Rc<Cell>`: no atomics, no locks.

use std::cell::Cell;
use std::rc::Rc;

/// Shared aggregate budget; clone one per connection on the owner thread.
#[derive(Clone)]
pub(crate) struct ParserBudget {
    held: Rc<Cell<usize>>,
    limit: usize,
}

/// One connection's share of the budget, released on drop.
pub(crate) struct ParserCharge {
    budget: ParserBudget,
    charged: usize,
}

impl ParserBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            held: Rc::new(Cell::new(0)),
            limit,
        }
    }

    pub(crate) fn charge(&self) -> ParserCharge {
        ParserCharge {
            budget: self.clone(),
            charged: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.held.get()
    }
}

impl ParserCharge {
    /// Record that this connection's parser now holds `bytes`. Fails when the
    /// aggregate would exceed the budget; the caller rejects this connection,
    /// whose share is released when the charge drops.
    pub(crate) fn update(&mut self, bytes: usize) -> Result<(), ()> {
        let held = self.budget.held.get() - self.charged + bytes;
        self.budget.held.set(held);
        self.charged = bytes;
        if held > self.budget.limit {
            Err(())
        } else {
            Ok(())
        }
    }

    /// Shrink this connection's share after its completed messages moved to
    /// the handoff budget. Never refuses: it only releases bytes.
    pub(crate) fn release_to(&mut self, bytes: usize) {
        let bytes = bytes.min(self.charged);
        self.budget
            .held
            .set(self.budget.held.get() - self.charged + bytes);
        self.charged = bytes;
    }
}

impl Drop for ParserCharge {
    fn drop(&mut self) {
        let held = self.budget.held.get() - self.charged;
        self.budget.held.set(held);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_sum_across_connections_and_release_on_drop() {
        let budget = ParserBudget::new(1_000);
        let mut first = budget.charge();
        let mut second = budget.charge();
        assert!(first.update(400).is_ok());
        assert!(second.update(500).is_ok());
        assert_eq!(budget.held(), 900);

        assert!(first.update(100).is_ok(), "a shrinking parser frees budget");
        assert_eq!(budget.held(), 600);

        second.release_to(200);
        assert_eq!(budget.held(), 300, "release_to only shrinks a share");
        second.release_to(900);
        assert_eq!(budget.held(), 300, "release_to never grows a share");

        drop(second);
        assert_eq!(budget.held(), 100);
        drop(first);
        assert_eq!(budget.held(), 0);
    }

    #[test]
    fn the_connection_that_would_exceed_the_budget_is_refused() {
        let budget = ParserBudget::new(1_000);
        let mut steady = budget.charge();
        let mut greedy = budget.charge();
        assert!(steady.update(600).is_ok());
        assert!(greedy.update(500).is_err());
        drop(greedy);
        assert_eq!(
            budget.held(),
            600,
            "the refused connection's share is released"
        );
        assert!(steady.update(1_000).is_ok());
    }
}
