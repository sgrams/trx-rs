// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

mod check;
mod detect;
mod prompts;
mod writer;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "trx-configurator",
    about = "Interactive configuration generator for trx-rs"
)]
struct Cli {
    /// Generate a default config without interactive prompts
    #[arg(long)]
    defaults: bool,

    /// Config type to generate (server, client, combined)
    #[arg(long, value_name = "TYPE")]
    r#type: Option<String>,

    /// Output file path (default: based on config type)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Check an existing config file for syntax and structure errors
    #[arg(long, value_name = "FILE")]
    check: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigType {
    Server,
    Client,
    Combined,
}

impl ConfigType {
    fn default_filename(&self) -> &'static str {
        match self {
            Self::Server => "trx-server.toml",
            Self::Client => "trx-client.toml",
            Self::Combined => "trx-rs.toml",
        }
    }
}

fn main() {
    let cli = Cli::parse();

    if let Some(path) = &cli.check {
        match check::check_file(path) {
            Ok(report) => {
                println!("{}", report);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }

    let config_type = if let Some(t) = &cli.r#type {
        match t.as_str() {
            "server" => ConfigType::Server,
            "client" => ConfigType::Client,
            "combined" => ConfigType::Combined,
            other => {
                eprintln!(
                    "Unknown config type '{}'. Use: server, client, combined",
                    other
                );
                std::process::exit(1);
            }
        }
    } else if cli.defaults {
        eprintln!("--defaults requires --type (server, client, combined)");
        std::process::exit(1);
    } else {
        prompts::prompt_config_type()
    };

    let output = cli
        .output
        .unwrap_or_else(|| PathBuf::from(config_type.default_filename()));

    let doc = if cli.defaults {
        writer::build_default(config_type)
    } else {
        match config_type {
            ConfigType::Server => {
                let general = prompts::prompt_server_general();
                let rig = prompts::prompt_rig();
                let listen = prompts::prompt_listen();
                writer::build_server(general, rig, listen)
            }
            ConfigType::Client => {
                let general = prompts::prompt_client_general();
                let remote = prompts::prompt_remote();
                let frontends = prompts::prompt_frontends();
                writer::build_client(general, remote, frontends)
            }
            ConfigType::Combined => {
                println!("\n--- Server configuration ---\n");
                let s_general = prompts::prompt_server_general();
                let rig = prompts::prompt_rig();
                let listen = prompts::prompt_listen();

                println!("\n--- Client configuration ---\n");
                let c_general = prompts::prompt_client_general();
                let remote = prompts::prompt_remote();
                let frontends = prompts::prompt_frontends();

                writer::build_combined(s_general, rig, listen, c_general, remote, frontends)
            }
        }
    };

    if let Err(e) = writer::write_file(&doc, &output) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
