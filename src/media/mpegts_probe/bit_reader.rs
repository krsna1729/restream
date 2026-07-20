pub(super) struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn remaining_bits(&self) -> usize {
        self.data
            .len()
            .saturating_sub(self.byte_pos)
            .saturating_mul(8)
            .saturating_sub(self.bit_pos as usize)
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.byte_pos >= self.data.len() {
            return None;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Some(bit as u32)
    }

    pub(super) fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 || self.remaining_bits() < n as usize {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            val = (val << 1) | self.read_bit()?;
        }
        Some(val)
    }

    pub(super) fn skip(&mut self, n: u32) -> Option<()> {
        if self.remaining_bits() < n as usize {
            return None;
        }
        for _ in 0..n {
            self.read_bit()?;
        }
        Some(())
    }

    pub(super) fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while self.read_bit()? == 0 {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        (1u32.checked_shl(leading_zeros)?)
            .checked_sub(1)?
            .checked_add(suffix)
    }

    pub(super) fn read_se(&mut self) -> Option<i32> {
        let ue = self.read_ue()?;
        if ue.is_multiple_of(2) {
            let magnitude = i32::try_from(ue / 2).ok()?;
            Some(-magnitude)
        } else {
            i32::try_from(ue / 2 + 1).ok()
        }
    }
}
