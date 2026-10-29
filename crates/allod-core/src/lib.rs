//! Business rules. M1: first-run setup and node credentials (SPEC §3.2–3.3, §2.3.3).
//! Ledger, payments, budgets, permissions and accounts follow in M3–M6.

pub mod apps;
pub mod fastsync;
pub mod fiat;
pub mod setup;
pub mod wallet;

pub use apps::{App, AppError, AppService};
pub use fiat::{FiatError, FiatService, RateSource, Rates};
pub use wallet::{Allocation, Wallet, WalletConfig, WalletError, WalletEvent};

pub use setup::{
    BitcoindRpc, ProvisionState, SetupConfig, SetupError, SetupOutcome, SetupProgress,
    SetupRequest, SetupService,
};
