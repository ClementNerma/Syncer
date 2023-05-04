use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[clap(help = "Path to the data directory")]
    pub data_dir: PathBuf,

    #[clap(help = "Address to serve at")]
    pub server_address: SocketAddr,

    #[clap(short, long, help = "Secret password")]
    pub secret: String,

    #[clap(global = true, short, long, help = "Show debug messages")]
    pub verbose: bool,
    // #[clap(global = true, short, long, help = "Show messages datetime")]
    // pub show_logs_timestamp: bool,
}
