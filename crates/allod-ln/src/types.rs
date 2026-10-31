//! Backend-neutral Lightning types (SPEC §5.1).

use serde::{Deserialize, Serialize};

/// Size of the validated public routing graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphStats {
    pub channels: u64,
    pub nodes: u64,
}

/// Channel as Allod shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub channel_id: String,
    pub user_channel_id: String,
    pub counterparty_node_id: String,
    pub short_channel_id: Option<u64>,
    pub funding_txid: Option<String>,
    pub capacity_sat: u64,
    pub outbound_capacity_msat: u64,
    pub inbound_capacity_msat: u64,
    pub is_outbound: bool,
    pub is_channel_ready: bool,
    pub is_usable: bool,
    pub is_announced: bool,
    pub confirmations: Option<u32>,
    pub confirmations_required: Option<u32>,
    pub fee_base_msat: Option<u32>,
    pub fee_ppm: Option<u32>,
    pub cltv_expiry_delta: Option<u32>,
    pub force_close_spend_delay: Option<u32>,
}

/// Peer connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: String,
    pub address: String,
    pub is_persisted: bool,
    pub is_connected: bool,
}

/// One entry of the Lightning balance breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceItem {
    /// e.g. "claimable_on_channel_close", "pending_broadcast".
    pub kind: String,
    pub channel_id: Option<String>,
    pub amount_sat: u64,
}

/// Node balances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balances {
    pub onchain_total_sat: u64,
    pub onchain_spendable_sat: u64,
    pub anchor_reserve_sat: u64,
    pub lightning_total_sat: u64,
    pub lightning: Vec<BalanceItem>,
    pub pending_sweeps: Vec<BalanceItem>,
}

/// Request to open a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenChannelReq {
    pub node_pubkey: String,
    pub address: String,
    /// None = all on-chain funds.
    pub amount_sat: Option<u64>,
    pub push_msat: Option<u64>,
    pub announce: bool,
    pub fee_base_msat: Option<u32>,
    pub fee_ppm: Option<u32>,
}

/// Forwarding fee update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelFees {
    pub fee_base_msat: Option<u32>,
    pub fee_ppm: Option<u32>,
    pub cltv_expiry_delta: Option<u32>,
}

/// Payment direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Inbound,
    Outbound,
}

/// Payment status in the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LnStatus {
    Pending,
    Succeeded,
    Failed,
}

/// What kind of payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LnPaymentKind {
    Onchain {
        txid: String,
        confirmed_height: Option<u32>,
    },
    Bolt11 {
        payment_hash: String,
        preimage: Option<String>,
    },
    Bolt12Offer {
        payment_hash: Option<String>,
        preimage: Option<String>,
        offer_id: String,
        payer_note: Option<String>,
        quantity: Option<u64>,
    },
    Bolt12Refund {
        payment_hash: Option<String>,
        preimage: Option<String>,
    },
    Spontaneous {
        payment_hash: String,
        preimage: Option<String>,
    },
}

impl LnPaymentKind {
    pub fn payment_hash(&self) -> Option<&str> {
        match self {
            LnPaymentKind::Bolt11 { payment_hash, .. }
            | LnPaymentKind::Spontaneous { payment_hash, .. } => Some(payment_hash),
            LnPaymentKind::Bolt12Offer { payment_hash, .. }
            | LnPaymentKind::Bolt12Refund { payment_hash, .. } => payment_hash.as_deref(),
            LnPaymentKind::Onchain { .. } => None,
        }
    }

    pub fn preimage(&self) -> Option<&str> {
        match self {
            LnPaymentKind::Bolt11 { preimage, .. }
            | LnPaymentKind::Spontaneous { preimage, .. }
            | LnPaymentKind::Bolt12Offer { preimage, .. }
            | LnPaymentKind::Bolt12Refund { preimage, .. } => preimage.as_deref(),
            LnPaymentKind::Onchain { .. } => None,
        }
    }
}

/// A payment as the backend reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LnPayment {
    pub id: String,
    pub kind: LnPaymentKind,
    pub direction: Direction,
    pub status: LnStatus,
    pub amount_msat: Option<u64>,
    pub fee_msat: Option<u64>,
    pub updated_at: u64,
}

/// Decoded BOLT11 invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedInvoice {
    pub payment_hash: String,
    pub destination: String,
    pub amount_msat: Option<u64>,
    pub description: Option<String>,
    pub description_hash: Option<String>,
    pub timestamp: u64,
    pub expiry_secs: u64,
    /// "bitcoin", "testnet", "signet", "regtest".
    pub network: String,
    pub is_expired: bool,
}

/// Decoded BOLT12 offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedOffer {
    pub offer_id: String,
    pub description: Option<String>,
    pub issuer: Option<String>,
    /// None = variable amount; Some(None) never: non-BTC currency offers set `currency`.
    pub amount_msat: Option<u64>,
    pub currency: Option<String>,
    pub absolute_expiry: Option<u64>,
    pub chains: Vec<String>,
    pub is_expired: bool,
}

/// A created BOLT11 invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedInvoice {
    pub invoice: String,
    pub payment_hash: String,
}

/// A created BOLT12 offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedOffer {
    pub offer: String,
    pub offer_id: String,
}

/// Invoice description: text or the SHA-256 of a longer description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceDescription {
    Direct(String),
    Hash(String),
}

/// On-chain send amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnchainAmount {
    Sats(u64),
    All,
}

/// Forwarded payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    pub id: String,
    pub prev_channel_id: String,
    pub next_channel_id: String,
    pub inbound_amount_msat: Option<u64>,
    pub outbound_amount_msat: Option<u64>,
    pub fee_earned_msat: Option<u64>,
    pub forwarded_at: u64,
}

/// Events from the backend (SPEC §5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LnEvent {
    PaymentReceived {
        payment: LnPayment,
    },
    PaymentSuccessful {
        payment: LnPayment,
        preimage: Option<String>,
        bolt12_invoice: Option<String>,
    },
    PaymentFailed {
        payment: LnPayment,
        reason: Option<String>,
    },
    PaymentClaimable {
        payment: LnPayment,
        claim_deadline: Option<u32>,
        claimable_amount_msat: u64,
    },
    PaymentForwarded,
    ChannelStateChanged {
        channel_id: String,
        counterparty_node_id: Option<String>,
        state: String,
        /// Why the channel closed, as ldk-server reports it (for example
        /// "Channel closed because the peer disconnected prior to the channel being funded").
        reason: Option<String>,
    },
    /// The event stream (re)connected; missed events must be reconciled.
    StreamConnected,
}

/// What this backend supports (SPEC §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub bolt12: bool,
    pub hold_invoices: bool,
    pub splicing: bool,
    pub force_close: bool,
    /// Receive via an LSPS2 just-in-time channel; true only while an LSP is configured.
    pub jit_receive: bool,
    pub payer_proof: bool,
    pub offer_issuer: bool,
    pub single_use_offers: bool,
}
