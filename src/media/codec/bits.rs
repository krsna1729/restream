//! Bit-level reading of untrusted H.264/H.265 parameter sets: one checked
//! reader and one emulation-prevention remover for every ingest path (RTMP
//! FLV probes, the MPEG-TS probe). A malformed input is `None`, never a
//! panic or a wrapped value.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

/// Most-significant-bit-first reader over an RBSP.
pub(crate) struct BitReader<'a> {
    /// Bytes not yet fully consumed; the first one is the current byte.
    data: &'a [u8],
    /// The next bit to read in the current byte (0x80 first, 0x01 last).
    mask: u8,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, mask: 0x80 }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let (&byte, rest) = self.data.split_first()?;
        let bit = byte & self.mask != 0;
        if self.mask == 0x01 {
            self.mask = 0x80;
            self.data = rest;
        } else {
            self.mask /= 2;
        }
        Some(bit)
    }

    /// Up to 32 bits as an unsigned value.
    pub(crate) fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            value = value.checked_mul(2)? | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    pub(crate) fn skip(&mut self, n: u32) -> Option<()> {
        for _ in 0..n {
            self.read_bit()?;
        }
        Some(())
    }

    /// ue(v): unsigned Exp-Golomb, codeNum up to 2^32 - 2.
    pub(crate) fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while !self.read_bit()? {
            leading_zeros = leading_zeros.checked_add(1)?;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        1u32.checked_shl(leading_zeros)?
            .checked_sub(1)?
            .checked_add(suffix)
    }

    /// se(v): codeNum k maps to (-1)^(k+1) * ceil(k / 2), computed in i64
    /// because codeNum reaches 2^32 - 2.
    pub(crate) fn read_se(&mut self) -> Option<i32> {
        let code = i64::from(self.read_ue()?);
        let magnitude = code.checked_add(1)? / 2;
        let value = if code & 1 == 1 {
            magnitude
        } else {
            magnitude.checked_neg()?
        };
        i32::try_from(value).ok()
    }
}

/// The RBSP of a NAL unit payload: every 0x03 that follows two zero bytes
/// (an emulation-prevention byte) removed.
pub(crate) fn rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u8;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 {
            zeros.saturating_add(1)
        } else {
            0
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_codes_read_back() {
        // ue: 1 → 0, 010 → 1, 011 → 2, 00100 → 3
        let mut r = BitReader::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!(r.read_ue(), Some(0));
        assert_eq!(r.read_ue(), Some(1));
        assert_eq!(r.read_ue(), Some(2));
        assert_eq!(r.read_ue(), Some(3));

        // se: 010 → 1, 011 → -1, 00100 → 2
        let mut r = BitReader::new(&[0b0100_1100, 0b1000_0000]);
        assert_eq!(r.read_se(), Some(1));
        assert_eq!(r.read_se(), Some(-1));
        assert_eq!(r.read_se(), Some(2));
    }

    /// The largest codeNum (2^32 - 2, 31 leading zeros) is -(2^31 - 1); it
    /// used to wrap through `as i32` to +1 in the RTMP copy of this reader.
    #[test]
    fn signed_exp_golomb_covers_the_full_code_range() {
        let mut r = BitReader::new(&[0, 0, 0, 0b0000_0001, 0xFF, 0xFF, 0xFF, 0xFE]);
        assert_eq!(r.read_se(), Some(-i32::MAX));
    }

    #[test]
    fn reads_past_the_end_and_over_32_bits_fail() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read_bits(33), None);
        assert_eq!(r.read_bits(8), Some(0xFF));
        assert_eq!(r.read_bits(1), None);
        assert_eq!(BitReader::new(&[0, 0, 0, 0]).read_ue(), None);
    }

    #[test]
    fn emulation_prevention_bytes_are_removed() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 0, 3]), [0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(rbsp(&[0, 3, 0, 0, 0, 3]), [0, 3, 0, 0, 0]);
    }
}
