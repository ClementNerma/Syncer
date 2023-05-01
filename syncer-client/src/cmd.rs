use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[clap(help = "Path to the data directory")]
    pub data_dir: PathBuf,

    #[clap(help = "Address of the server to contact")]
    pub address: String,

    #[clap(global = true, short, long, help = "Display debug messages")]
    pub verbose: bool,
}
