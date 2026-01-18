// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[cfg(all(target_os = "linux", feature = "qt"))]
pub mod server;

#[cfg(all(target_os = "linux", feature = "qt"))]
pub fn register_frontend() {
    use trx_frontend::FrontendSpawner;
    trx_frontend::register_frontend("qt", server::QtFrontend::spawn_frontend);
}

#[cfg(not(all(target_os = "linux", feature = "qt")))]
pub fn register_frontend() {
    // No-op on non-Linux platforms.
}
