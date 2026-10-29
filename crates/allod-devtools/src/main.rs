//! `allod-devtools`: helpers for the regtest stack and CI (SPEC §2.3.9).

mod check;
mod nwc;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "allod-devtools", version, about = "Allod development helpers")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that every service of the stack is up and wired together.
    Check(check::CheckArgs),
    /// Print a fresh 24-word BIP39 mnemonic.
    GenMnemonic,
    /// Create a NWC connection on a regtest Allod and print its URI.
    NwcUri(nwc::UriArgs),
    /// Send one NWC request and print the response.
    Nwc(nwc::CallArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Check(args) => check::run(args).await,
        Command::NwcUri(a) => nwc::uri(a).await,
        Command::Nwc(a) => nwc::call(a).await,
        Command::GenMnemonic => {
            let mnemonic = bip39::Mnemonic::generate(24)?;
            println!("{mnemonic}");
            Ok(())
        }
    }
}
