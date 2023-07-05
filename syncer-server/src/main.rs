#![forbid(unsafe_code)]
#![forbid(unused_must_use)]
#![warn(unused_crate_dependencies)]

use openssl as _;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use cmd::Args;
use server::serve;
use tracing::metadata::LevelFilter;
use tracing_subscriber::fmt::time::SystemTime;

mod cmd;
mod server;

#[tokio::main]
async fn main() {
    if let Err(err) = inner_main().await {
        tracing::error!("{err:?}");
        std::process::exit(1);
    }
}

async fn inner_main() -> Result<()> {
    let Args {
        data_dir,
        server_address,
        verbose,
        secret,
    } = Args::parse();

    tracing_subscriber::fmt()
        .with_target(false)
        .with_timer(SystemTime)
        .with_max_level(if verbose {
            LevelFilter::DEBUG
        } else {
            LevelFilter::INFO
        })
        .compact()
        .init();

    serve(&server_address, secret, PathBuf::from(&data_dir)).await
}
