// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod server;

pub fn register_frontend_on(context: &mut trx_frontend::FrontendRegistrationContext) {
    use trx_frontend::FrontendSpawner;
    context.register_frontend("http-json", server::HttpJsonFrontend::spawn_frontend);
}
