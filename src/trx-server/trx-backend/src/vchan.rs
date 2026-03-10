// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

// Re-export the trait and types from trx-core so crates that depend on
// trx-backend can use them without a direct trx-core dependency.
pub use trx_core::vchan::{SharedVChanManager, VChanError, VChannelInfo, VirtualChannelManager};
