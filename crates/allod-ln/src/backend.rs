use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::types::*;
use crate::NodeInfo;

/// Backend errors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// Credentials or certificate could not be read or parsed.
    #[error("invalid ldk-server credentials: {0}")]
    Credentials(String),
    /// The request was invalid (bad invoice, unknown peer, ...).
    #[error("{0}")]
    InvalidRequest(String),
    /// Lightning-level failure (no route, payment failed, ...).
    #[error("{0}")]
    Lightning(String),
    /// Not permitted by the macaroon.
    #[error("not permitted: {0}")]
    Permission(String),
    /// Transport or server failure; may be retried.
    #[error("ldk-server unavailable: {0}")]
    Unavailable(String),
    /// The RPC did not finish in time.
    #[error("ldk-server request timed out")]
    Timeout,
    /// Feature not supported by this backend.
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

pub type BackendResult<T> = Result<T, BackendError>;

/// Everything Allod needs from a Lightning node (SPEC §5.1).
#[async_trait]
pub trait LightningBackend: Send + Sync + 'static {
    fn capabilities(&self) -> Capabilities;
    /// Fan-out of backend events.
    fn subscribe(&self) -> broadcast::Receiver<LnEvent>;

    async fn node_info(&self) -> BackendResult<NodeInfo>;
    async fn graph_stats(&self) -> BackendResult<GraphStats>;
    async fn balances(&self) -> BackendResult<Balances>;

    // channels & peers
    async fn list_channels(&self) -> BackendResult<Vec<Channel>>;
    async fn open_channel(&self, req: OpenChannelReq) -> BackendResult<String>;
    async fn close_channel(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        force: bool,
        reason: Option<String>,
    ) -> BackendResult<()>;
    async fn update_channel_fees(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        fees: ChannelFees,
    ) -> BackendResult<()>;
    async fn splice_in(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        amount_sat: u64,
    ) -> BackendResult<()>;
    async fn splice_out(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        address: Option<String>,
        amount_sat: u64,
    ) -> BackendResult<()>;
    async fn list_peers(&self) -> BackendResult<Vec<Peer>>;
    async fn connect_peer(&self, node_id: &str, address: &str, persist: bool) -> BackendResult<()>;
    async fn disconnect_peer(&self, node_id: &str) -> BackendResult<()>;

    // on-chain
    async fn onchain_receive(&self) -> BackendResult<String>;
    async fn onchain_send(
        &self,
        address: &str,
        amount: OnchainAmount,
        fee_rate_sat_vb: Option<u64>,
    ) -> BackendResult<String>;
    async fn onchain_bump_fee(
        &self,
        payment_id: &str,
        fee_rate_sat_vb: Option<u64>,
    ) -> BackendResult<String>;

    // misc
    async fn sign_message(&self, message: &[u8]) -> BackendResult<String>;
    async fn verify_signature(
        &self,
        message: &[u8],
        signature: &str,
        pubkey: &str,
    ) -> BackendResult<bool>;
    async fn list_forwards(
        &self,
        page_token: Option<String>,
    ) -> BackendResult<(Vec<Forward>, Option<String>)>;

    // BOLT11
    async fn bolt11_receive(
        &self,
        amount_msat: Option<u64>,
        description: InvoiceDescription,
        expiry_secs: u32,
    ) -> BackendResult<CreatedInvoice>;
    /// Creates a fixed-amount invoice paid through an LSPS2 just-in-time channel
    /// (SPEC §5.1): when the payer pays it, the LSP opens an inbound channel and
    /// forwards over it. Requires a configured LSP (see `Capabilities::jit_receive`);
    /// `max_lsp_fee_msat` caps the total fee the LSP may deduct for opening it.
    async fn bolt11_receive_jit(
        &self,
        amount_msat: u64,
        description: InvoiceDescription,
        expiry_secs: u32,
        max_lsp_fee_msat: Option<u64>,
    ) -> BackendResult<CreatedInvoice>;
    async fn bolt11_receive_for_hash(
        &self,
        amount_msat: Option<u64>,
        description: InvoiceDescription,
        expiry_secs: u32,
        payment_hash: &str,
    ) -> BackendResult<String>;
    async fn bolt11_claim_for_id(
        &self,
        payment_id: &str,
        claimable_amount_msat: u64,
        preimage: &str,
    ) -> BackendResult<()>;
    async fn bolt11_fail_for_id(&self, payment_id: &str) -> BackendResult<()>;
    async fn bolt11_send(
        &self,
        invoice: &str,
        amount_msat: Option<u64>,
        max_fee_msat: Option<u64>,
    ) -> BackendResult<String>;
    async fn decode_invoice(&self, invoice: &str) -> BackendResult<DecodedInvoice>;

    // BOLT12
    async fn bolt12_receive(
        &self,
        description: &str,
        amount_msat: Option<u64>,
        expiry_secs: Option<u32>,
    ) -> BackendResult<CreatedOffer>;
    async fn decode_offer(&self, offer: &str) -> BackendResult<DecodedOffer>;
    async fn bolt12_send(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
        max_fee_msat: Option<u64>,
    ) -> BackendResult<String>;
    async fn bolt12_payer_proof(
        &self,
        payment_id: &str,
        preimage: &str,
        invoice: &str,
    ) -> BackendResult<String>;

    // payments
    async fn payment(&self, id: &str) -> BackendResult<Option<LnPayment>>;
    async fn list_payments(
        &self,
        page_token: Option<String>,
    ) -> BackendResult<(Vec<LnPayment>, Option<String>)>;
}
