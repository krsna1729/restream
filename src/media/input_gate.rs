use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use crate::media::ring_buffer::MediaPacket;

const STANDBY: u8 = 0;
const AWAITING_KEYFRAME: u8 = 1;
const ACTIVE: u8 = 2;
const STATE_MASK: usize = 0b11;
const IN_FLIGHT_ONE: usize = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputForwardState {
    Standby,
    AwaitingKeyframe,
    Active,
}

impl InputForwardState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standby => "standby",
            Self::AwaitingKeyframe => "awaitingKeyframe",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPacketBoundary {
    VideoKeyframe,
    ReplayReady,
    Other,
}

#[derive(Debug)]
pub struct InputPacketGate {
    state_and_in_flight: AtomicUsize,
}

impl InputPacketGate {
    pub const fn standby() -> Self {
        Self {
            state_and_in_flight: AtomicUsize::new(STANDBY as usize),
        }
    }

    pub const fn active() -> Self {
        Self {
            state_and_in_flight: AtomicUsize::new(ACTIVE as usize),
        }
    }

    pub fn state(&self) -> InputForwardState {
        match self.state_and_in_flight.load(Ordering::SeqCst) & STATE_MASK {
            state if state == STANDBY as usize => InputForwardState::Standby,
            state if state == AWAITING_KEYFRAME as usize => InputForwardState::AwaitingKeyframe,
            state if state == ACTIVE as usize => InputForwardState::Active,
            _ => InputForwardState::Standby,
        }
    }

    pub fn activate(&self) {
        self.set_state(ACTIVE);
    }

    pub fn arm_for_promotion(&self) {
        self.set_state(AWAITING_KEYFRAME);
    }

    pub fn demote(&self) {
        self.set_state(STANDBY);
    }

    pub fn try_enter(&self, boundary: InputPacketBoundary) -> Option<InputPacketLease<'_>> {
        loop {
            let current = self.state_and_in_flight.load(Ordering::SeqCst);
            let state = current & STATE_MASK;
            let activated = match state {
                state if state == STANDBY as usize => return None,
                state if state == AWAITING_KEYFRAME as usize => {
                    if boundary == InputPacketBoundary::Other {
                        return None;
                    }
                    true
                }
                state if state == ACTIVE as usize => false,
                _ => return None,
            };
            let next_state = if activated { ACTIVE as usize } else { state };
            let next = (current & !STATE_MASK).checked_add(IN_FLIGHT_ONE)? | next_state;
            if self
                .state_and_in_flight
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(InputPacketLease {
                    gate: self,
                    activated,
                });
            }
        }
    }

    pub async fn wait_until_idle(&self) {
        while self.state_and_in_flight.load(Ordering::SeqCst) > STATE_MASK {
            tokio::task::yield_now().await;
        }
    }

    fn set_state(&self, state: u8) {
        let state = state as usize;
        let mut current = self.state_and_in_flight.load(Ordering::SeqCst);
        loop {
            let next = (current & !STATE_MASK) | state;
            match self.state_and_in_flight.compare_exchange(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

pub struct InputPacketLease<'a> {
    gate: &'a InputPacketGate,
    activated: bool,
}

impl InputPacketLease<'_> {
    pub fn activated(&self) -> bool {
        self.activated
    }
}

impl Drop for InputPacketLease<'_> {
    fn drop(&mut self) {
        self.gate
            .state_and_in_flight
            .fetch_sub(IN_FLIGHT_ONE, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
pub struct InputTimestampMapper {
    initialized: bool,
    offset_ms: i64,
}

impl InputTimestampMapper {
    pub fn map_packet(
        &mut self,
        packet: &mut MediaPacket,
        activated: bool,
        last_forwarded_dts: &AtomicI64,
    ) {
        if activated || !self.initialized {
            self.offset_ms = if activated {
                let previous = last_forwarded_dts.load(Ordering::Acquire);
                if previous == i64::MIN {
                    0
                } else {
                    previous.saturating_add(1).saturating_sub(packet.dts)
                }
            } else {
                0
            };
            self.initialized = true;
        }
        packet.dts = packet.dts.saturating_add(self.offset_ms);
        packet.pts = packet.pts.saturating_add(self.offset_ms);
    }

    pub fn record_forwarded(packet: &MediaPacket, last_forwarded_dts: &AtomicI64) {
        last_forwarded_dts.fetch_max(packet.dts, Ordering::Release);
    }
}
