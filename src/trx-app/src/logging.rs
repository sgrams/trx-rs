// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use tracing::Level;
use tracing_subscriber::FmtSubscriber;

/// Initialize logging with optional level from config.
/// Falls back to INFO if level is None or invalid.
pub fn init_logging(log_level: Option<&str>) {
    let level = log_level
        .and_then(|s| s.parse::<Level>().ok())
        .unwrap_or(Level::INFO);

    FmtSubscriber::builder()
        .with_target(false)
        .with_max_level(level)
        .init();
}
