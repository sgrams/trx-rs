// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod server;

pub fn register_frontend() {
    use trx_frontend::FrontendSpawner;
    trx_frontend::register_frontend("http-json", server::HttpJsonFrontend::spawn_frontend);
}

pub fn set_auth_tokens(tokens: Vec<String>) {
    server::set_auth_tokens(tokens);
}
