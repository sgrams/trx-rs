// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[cfg(all(target_os = "macos", feature = "appkit"))]
pub mod helpers;
#[cfg(all(target_os = "macos", feature = "appkit"))]
pub mod model;
#[cfg(all(target_os = "macos", feature = "appkit"))]
pub mod server;
#[cfg(all(target_os = "macos", feature = "appkit"))]
pub mod ui;

#[cfg(all(target_os = "macos", feature = "appkit"))]
pub use server::run_appkit_main_thread;

#[cfg(all(target_os = "macos", feature = "appkit"))]
pub fn register_frontend() {
    use trx_frontend::FrontendSpawner;
    trx_frontend::register_frontend("appkit", server::AppKitFrontend::spawn_frontend);
}

#[cfg(not(all(target_os = "macos", feature = "appkit")))]
pub fn register_frontend() {
    // No-op on non-macOS platforms or when appkit feature is disabled.
}
