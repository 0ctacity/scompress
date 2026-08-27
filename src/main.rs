mod applesauce;
mod cli;
mod model;
mod safety;
mod scanner;
mod tui;

use clap::Parser;
use cli::{Cli, Commands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let res = smol::block_on(async {
        match cli.command {
            Some(Commands::List { tool }) => cli::run_list(tool).await,
            Some(Commands::Compress { tool }) => cli::run_compress(tool).await,
            Some(Commands::Decompress { tool }) => cli::run_decompress(tool).await,
            None => tui::run_tui().await,
        }
    });

    if let Err(ref e) = res
        && let Some(io_err) = e.downcast_ref::<std::io::Error>()
        && io_err.kind() == std::io::ErrorKind::BrokenPipe
    {
        return Ok(());
    }

    res
}
