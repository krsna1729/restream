//! MPEG-TS continuity-counter check over delivered SRT payloads: the
//! receiver-side oracle for a lost stretch of one output's stream
//! (ISO/IEC 13818-1, 2.4.3.3). Restream's shared muxer runs each PID's
//! counter across everything one output sends, redials included, so a
//! missing packet in delivered order shows as a counter jump.

const TS_PACKET: usize = 188;
const SYNC_BYTE: u8 = 0x47;
const NULL_PID: usize = 0x1FFF;
const PID_COUNT: usize = 0x2000;

/// What one payload added.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TsTally {
    pub packets: u64,
    /// Counter jumps: packets lost (or reordered) before delivery.
    pub errors: u64,
    /// Payloads or packets that are not whole, sync-aligned TS packets.
    pub misaligned: u64,
}

/// Each PID's last continuity counter over one delivered byte stream.
pub(crate) struct TsContinuity {
    last: Box<[Option<u8>]>,
}

impl TsContinuity {
    pub(crate) fn new() -> Self {
        Self {
            last: vec![None; PID_COUNT].into_boxed_slice(),
        }
    }

    /// Check the next delivered SRT payload (whole TS packets).
    pub(crate) fn feed(&mut self, payload: &[u8]) -> TsTally {
        let mut tally = TsTally::default();
        if !payload.len().is_multiple_of(TS_PACKET) {
            tally.misaligned += 1;
            return tally;
        }
        for packet in payload.chunks_exact(TS_PACKET) {
            if packet[0] != SYNC_BYTE {
                tally.misaligned += 1;
                continue;
            }
            tally.packets += 1;
            tally.errors += u64::from(self.jumped(packet));
        }
        tally
    }

    fn jumped(&mut self, packet: &[u8]) -> bool {
        let pid = usize::from(packet[1] & 0x1F) << 8 | usize::from(packet[2]);
        let control = (packet[3] >> 4) & 0b11;
        // Without a payload the counter does not advance.
        if pid == NULL_PID || control & 0b01 == 0 {
            return false;
        }
        let counter = packet[3] & 0x0F;
        let restarts = control & 0b10 != 0 && packet[4] > 0 && packet[5] & 0x80 != 0;
        let previous = self.last[pid].replace(counter);
        // One duplicate packet is legal and loses nothing.
        previous.is_some_and(|previous| {
            !restarts && counter != previous && counter != (previous + 1) & 0x0F
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload-only packet on `pid` with `counter`.
    fn packet(pid: u16, counter: u8) -> Vec<u8> {
        let mut packet = vec![0xFF; TS_PACKET];
        packet[..4].copy_from_slice(&[
            SYNC_BYTE,
            (pid >> 8) as u8 & 0x1F,
            pid as u8,
            0x10 | (counter & 0x0F),
        ]);
        packet
    }

    #[test]
    fn a_lost_packet_is_one_error_across_the_counter_wrap() {
        let mut continuity = TsContinuity::new();
        let mut stream: Vec<u8> = (0..20).flat_map(|n| packet(0x100, n)).collect();
        assert_eq!(
            continuity.feed(&stream[..TS_PACKET * 7]),
            TsTally {
                packets: 7,
                ..TsTally::default()
            }
        );
        // Lose counter 9 (packet 9); 15 -> 0 wraps legally.
        stream.drain(TS_PACKET * 9..TS_PACKET * 10);
        let tally = continuity.feed(&stream[TS_PACKET * 7..]);
        assert_eq!((tally.packets, tally.errors), (12, 1));
    }

    #[test]
    fn pids_are_independent_and_duplicates_lose_nothing() {
        let mut continuity = TsContinuity::new();
        let stream: Vec<u8> = [packet(0x100, 3), packet(0x101, 9), packet(0x100, 3)]
            .concat()
            .into_iter()
            .chain(packet(0x100, 4))
            .chain(packet(0x101, 10))
            .collect();
        assert_eq!(continuity.feed(&stream).errors, 0);
    }

    #[test]
    fn adaptation_only_packets_hold_and_the_discontinuity_flag_restarts() {
        let mut continuity = TsContinuity::new();
        let mut adaptation_only = packet(0x100, 5);
        adaptation_only[3] = 0x25; // adaptation field only, counter unchanged
        let mut restart = packet(0x100, 0);
        restart[3] = 0x30; // adaptation field and payload, counter 0
        restart[4] = 1;
        restart[5] = 0x80; // discontinuity_indicator
        let stream = [packet(0x100, 5), adaptation_only, packet(0x100, 6), restart].concat();
        assert_eq!(continuity.feed(&stream).errors, 0);
        assert_eq!(continuity.feed(&packet(0x100, 2)).errors, 1);
    }

    #[test]
    fn a_payload_that_is_not_whole_ts_packets_is_misaligned() {
        let mut continuity = TsContinuity::new();
        assert_eq!(continuity.feed(&[SYNC_BYTE; 100]).misaligned, 1);
        let mut unsynced = packet(0x100, 0);
        unsynced[0] = 0;
        let tally = continuity.feed(&unsynced);
        assert_eq!((tally.packets, tally.misaligned), (0, 1));
    }
}
