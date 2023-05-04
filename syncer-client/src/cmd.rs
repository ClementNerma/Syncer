use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[clap(help = "Path to the data directory")]
    pub data_dir: PathBuf,

    #[clap(help = "Address of the server to contact")]
    pub address: String,

    #[clap(long, help = "Server's secret password")]
    pub server_secret: String,

    #[clap(
        short,
        long,
        help = "Maximum number of parallel transfers (default: 8)",
        default_value = "8"
    )]
    pub max_parallel_transfers: usize,

    #[clap(
        short,
        long,
        help = "Item names to ignore (start with a '/' for root-only)"
    )]
    pub ignore_items: Vec<String>,

    #[clap(long, help = "File extensions to ignore")]
    pub ignore_exts: Vec<String>,

    #[clap(global = true, short, long, help = "Display debug messages")]
    pub verbose: bool,
}
