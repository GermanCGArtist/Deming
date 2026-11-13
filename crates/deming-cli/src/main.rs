//! Unified entrypoint for the `deming` CLI, MCP server, and desktop GUI.

use clap::Parser;
use deming_cli::cli::{dispatch, Cli};

fn main() {
    if std::env::args().any(|a| a == "--login-popup") {
        if let Err(err) = deming_gui::run_gui(None) {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
        return;
    }

    let cli = Cli::parse();
    let json_mode = cli.json;

    if cli.command.is_none() {
        if let Err(err) = deming_gui::run_gui(cli.config) {
            if json_mode {
                let payload = serde_json::json!({ "error": format!("{err}") });
                eprintln!("{payload}");
            } else {
                eprintln!("error: {err}");
            }
            std::process::exit(1);
        }
        return;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("error: failed to initialize tokio runtime: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = rt.block_on(dispatch(cli)) {
        if json_mode {
            let payload = serde_json::json!({
                "error": format!("{err:#}")
            });
            eprintln!("{payload}");
        } else {
            eprintln!("error: {err:#}");
        }
        std::process::exit(1);
    }
}
