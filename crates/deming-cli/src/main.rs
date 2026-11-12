//! Entrypoint for the `deming` CLI binary.

use clap::Parser;
use deming_cli::cli::{dispatch, Cli};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json_mode = cli.json;

    if let Err(err) = dispatch(cli).await {
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
