// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

/// FTx protocol variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtxProtocol {
    Ft4,
    Ft8,
    Ft2,
}

impl FtxProtocol {
    /// Symbol period in seconds.
    pub fn symbol_period(self) -> f32 {
        match self {
            Self::Ft8 => FT8_SYMBOL_PERIOD,
            Self::Ft4 => FT4_SYMBOL_PERIOD,
            Self::Ft2 => FT2_SYMBOL_PERIOD,
        }
    }

    /// Slot time in seconds.
    pub fn slot_time(self) -> f32 {
        match self {
            Self::Ft8 => FT8_SLOT_TIME,
            Self::Ft4 => FT4_SLOT_TIME,
            Self::Ft2 => FT2_SLOT_TIME,
        }
    }

    /// Whether this protocol uses FT4-style channel layout (FT4 and FT2).
    pub fn uses_ft4_layout(self) -> bool {
        matches!(self, Self::Ft4 | Self::Ft2)
    }

    /// Number of data symbols.
    pub fn nd(self) -> usize {
        if self.uses_ft4_layout() { FT4_ND } else { FT8_ND }
    }

    /// Total channel symbols.
    pub fn nn(self) -> usize {
        if self.uses_ft4_layout() { FT4_NN } else { FT8_NN }
    }

    /// Length of each sync group.
    pub fn sync_length(self) -> usize {
        if self.uses_ft4_layout() { FT4_LENGTH_SYNC } else { FT8_LENGTH_SYNC }
    }

    /// Number of sync groups.
    pub fn num_sync(self) -> usize {
        if self.uses_ft4_layout() { FT4_NUM_SYNC } else { FT8_NUM_SYNC }
    }

    /// Offset between sync groups.
    pub fn sync_offset(self) -> usize {
        if self.uses_ft4_layout() { FT4_SYNC_OFFSET } else { FT8_SYNC_OFFSET }
    }

    /// Number of FSK tones.
    pub fn num_tones(self) -> usize {
        if self.uses_ft4_layout() { 4 } else { 8 }
    }
}

// FT8 timing
pub const FT8_SYMBOL_PERIOD: f32 = 0.160;
pub const FT8_SLOT_TIME: f32 = 15.0;

// FT4 timing
pub const FT4_SYMBOL_PERIOD: f32 = 0.048;
pub const FT4_SLOT_TIME: f32 = 7.5;

// FT2 timing
pub const FT2_SYMBOL_PERIOD: f32 = 0.024;
pub const FT2_SLOT_TIME: f32 = 3.75;

// FT8 symbol counts
pub const FT8_ND: usize = 58;
pub const FT8_NN: usize = 79;
pub const FT8_LENGTH_SYNC: usize = 7;
pub const FT8_NUM_SYNC: usize = 3;
pub const FT8_SYNC_OFFSET: usize = 36;

// FT4 symbol counts
pub const FT4_ND: usize = 87;
pub const FT4_NR: usize = 2;
pub const FT4_NN: usize = 105;
pub const FT4_LENGTH_SYNC: usize = 4;
pub const FT4_NUM_SYNC: usize = 4;
pub const FT4_SYNC_OFFSET: usize = 33;

// FT2 reuses FT4 layout
pub const FT2_ND: usize = FT4_ND;
pub const FT2_NR: usize = FT4_NR;
pub const FT2_NN: usize = FT4_NN;
pub const FT2_LENGTH_SYNC: usize = FT4_LENGTH_SYNC;
pub const FT2_NUM_SYNC: usize = FT4_NUM_SYNC;
pub const FT2_SYNC_OFFSET: usize = FT4_SYNC_OFFSET;

// LDPC parameters
pub const FTX_LDPC_N: usize = 174;
pub const FTX_LDPC_K: usize = 91;
pub const FTX_LDPC_M: usize = 83;
pub const FTX_LDPC_N_BYTES: usize = (FTX_LDPC_N + 7) / 8;
pub const FTX_LDPC_K_BYTES: usize = (FTX_LDPC_K + 7) / 8;

// CRC parameters
pub const FT8_CRC_POLYNOMIAL: u16 = 0x2757;
pub const FT8_CRC_WIDTH: u32 = 14;

// Message parameters
pub const FTX_PAYLOAD_LENGTH_BYTES: usize = 10;
pub const FTX_MAX_MESSAGE_LENGTH: usize = 35;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_timing() {
        assert!((FtxProtocol::Ft8.symbol_period() - 0.160).abs() < 1e-6);
        assert!((FtxProtocol::Ft4.symbol_period() - 0.048).abs() < 1e-6);
        assert!((FtxProtocol::Ft2.symbol_period() - 0.024).abs() < 1e-6);
    }

    #[test]
    fn ft4_layout() {
        assert!(FtxProtocol::Ft4.uses_ft4_layout());
        assert!(FtxProtocol::Ft2.uses_ft4_layout());
        assert!(!FtxProtocol::Ft8.uses_ft4_layout());
    }

    #[test]
    fn symbol_counts() {
        assert_eq!(FtxProtocol::Ft8.nn(), 79);
        assert_eq!(FtxProtocol::Ft4.nn(), 105);
        assert_eq!(FtxProtocol::Ft2.nn(), 105);
    }
}
