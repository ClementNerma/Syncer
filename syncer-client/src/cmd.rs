use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[clap(help = "Path to the data directory")]
    pub data_dir: PathBuf,

    #[clap(help = "Address of the server to contact")]
    pub address: String,

    #[clap(
        short,
        long,
        help = "Maximum number of parallel transfers (default: 8)",
        default_value = "8"
    )]
    pub max_parallel_transfers: usize,

    #[clap(global = true, short, long, help = "Display debug messages")]
    pub verbose: bool,
}
