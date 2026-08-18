use super::*;

impl<T> SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    pub(super) fn next_wakeup(&self) -> Option<Instant> {
        let timer_deadline = self.transport.next_timer_deadline();
        let send_deadline = self
            .engine
            .needs_write_interest()
            .then(|| self.transport.next_send_deadline())
            .flatten();
        timer_deadline.into_iter().chain(send_deadline).min()
    }

    pub(super) fn on_wakeup(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.next_wakeup() else {
            return false;
        };
        if deadline > now {
            return false;
        }
        let was_write_ready = self.transport.write_ready();
        self.transport.on_wakeup();
        self.transport.is_closed()
            || (self.engine.needs_write_interest()
                && !was_write_ready
                && self.transport.write_ready())
    }
}

pub(super) fn transport_wakeup<P, C, K, R>(backend: &mut SrtShardBackend<P, C, K, R>) -> usize
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
    let now = Instant::now();
    let mut wake_keys = Vec::new();
    for (index, leaf) in backend.leaves.iter_mut().enumerate() {
        let Some(leaf) = leaf.as_mut() else {
            continue;
        };
        if leaf.on_wakeup(now) {
            wake_keys.push(LeafKey(index));
        }
    }
    let mut scheduled = 0;
    for key in wake_keys {
        let Some(handle) = backend
            .output_sockets
            .values()
            .find(|socket| socket.key == key)
            .map(|socket| socket.handle)
        else {
            continue;
        };
        let Some(leaf) = backend.leaf_mut(key) else {
            continue;
        };
        if leaf.common.schedule.enqueued {
            continue;
        }
        leaf.common.schedule.enqueued = true;
        let generation = leaf.common.generation;
        backend.ready.push_back(SrtReadyLeaf {
            handle,
            key,
            generation,
            readable: false,
            writable: true,
        });
        scheduled += 1;
    }
    scheduled
}

pub(super) fn next_wakeup<P, C, K, R>(backend: &SrtShardBackend<P, C, K, R>) -> Option<Instant>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
    backend
        .leaves
        .iter()
        .filter_map(|leaf| leaf.as_ref().and_then(SrtFabricLeaf::next_wakeup))
        .min()
}
