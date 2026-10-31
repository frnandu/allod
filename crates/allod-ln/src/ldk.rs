//! ldk-server implementation of [`LightningBackend`] (SPEC §5.2) and the event hub (§5.3).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ldk_server_client::client::LdkServerClient;
use ldk_server_client::error::{LdkServerError, LdkServerErrorCode};
use ldk_server_client::ldk_server_grpc::api::*;
use ldk_server_client::ldk_server_grpc::events::{event_envelope, EventEnvelope};
use ldk_server_client::ldk_server_grpc::types as t;
use tokio::sync::broadcast;

use crate::backend::{BackendError, BackendResult, LightningBackend};
use crate::types::*;
use crate::{LdkServerConfig, NodeInfo};
use allod_types::{Network, NodeSummary};

const UNARY_TIMEOUT: Duration = Duration::from_secs(30);
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

fn map_err(e: LdkServerError) -> BackendError {
    match e.error_code {
        LdkServerErrorCode::InvalidRequestError => BackendError::InvalidRequest(e.message),
        LdkServerErrorCode::AuthError | LdkServerErrorCode::AuthorizationError => {
            BackendError::Permission(e.message)
        }
        LdkServerErrorCode::LightningError => BackendError::Lightning(e.message),
        LdkServerErrorCode::InternalServerError | LdkServerErrorCode::InternalError => {
            BackendError::Unavailable(e.message)
        }
    }
}

async fn call<T>(
    timeout: Duration,
    fut: impl Future<Output = Result<T, LdkServerError>>,
) -> BackendResult<T> {
    tokio::time::timeout(timeout, fut).await.map_err(|_| BackendError::Timeout)?.map_err(map_err)
}

/// Client for one ldk-server instance.
#[derive(Clone)]
pub struct LdkServerBackend {
    client: LdkServerClient,
    events: broadcast::Sender<LnEvent>,
    started: Arc<std::sync::Once>,
    /// LSPS2 LSP (`pubkey@host:port`) ldk-server is configured with; `None` disables
    /// JIT receive (SPEC §2.3). Allod mirrors its own setting here, because ldk-server
    /// does not expose its liquidity sources over gRPC.
    lsps2_lsp: Option<String>,
}

impl LdkServerBackend {
    /// Builds a client. Does not contact the server; call [`LdkServerBackend::start_events`]
    /// to start the event stream.
    pub fn new(config: &LdkServerConfig) -> BackendResult<Self> {
        let client = LdkServerClient::new(
            config.address.clone(),
            config.macaroon_hex.clone(),
            &config.tls_cert_pem,
        )
        .map_err(BackendError::Credentials)?;
        let (events, _) = broadcast::channel(1024);
        Ok(LdkServerBackend {
            client,
            events,
            started: Arc::new(std::sync::Once::new()),
            lsps2_lsp: None,
        })
    }

    /// Marks an LSPS2 LSP as configured, enabling just-in-time receive.
    pub fn with_lsps2_lsp(mut self, lsp: Option<String>) -> Self {
        self.lsps2_lsp = lsp;
        self
    }

    /// Starts the event stream task once (SPEC §5.3): reconnect with backoff, announce
    /// every (re)connect so callers reconcile missed events.
    pub fn start_events(&self) {
        let this = self.clone();
        self.started.call_once(move || {
            tokio::spawn(async move { this.event_loop().await });
        });
    }

    async fn event_loop(self) {
        let mut backoff = Duration::from_millis(500);
        loop {
            match self.client.subscribe_events().await {
                Ok(mut stream) => {
                    tracing::info!("ldk-server event stream connected");
                    backoff = Duration::from_millis(500);
                    let _ = self.events.send(LnEvent::StreamConnected);
                    while let Some(msg) = stream.next_message().await {
                        match msg {
                            Ok(env) => {
                                if let Some(ev) = convert_event(env) {
                                    let _ = self.events.send(ev);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "ldk-server event stream error");
                                break;
                            }
                        }
                    }
                    tracing::warn!("ldk-server event stream closed");
                }
                Err(e) => tracing::debug!(error = %e, "ldk-server event stream connect failed"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    /// Node info subset (used by health checks).
    pub async fn node_info(&self) -> BackendResult<NodeInfo> {
        let resp =
            call(UNARY_TIMEOUT, self.client.get_node_info(GetNodeInfoRequest::default())).await?;
        let best = resp.current_best_block.clone().unwrap_or_default();
        let network = match resp.network() {
            t::Network::Bitcoin => Network::Bitcoin,
            t::Network::Testnet => Network::Testnet,
            t::Network::Testnet4 => Network::Testnet4,
            t::Network::Signet => Network::Signet,
            t::Network::Regtest => Network::Regtest,
        };
        Ok(NodeInfo {
            summary: NodeSummary {
                pubkey: resp.node_id.clone(),
                block_height: best.height,
                block_hash: best.block_hash.to_string(),
            },
            network: Some(network),
            alias: resp.node_alias.clone(),
            version: resp.version.clone(),
            listening_addresses: resp.listening_addresses.clone(),
            announcement_addresses: resp.announcement_addresses.clone(),
            node_uris: resp.node_uris.clone(),
        })
    }

    /// Macaroon permissions (`GetPermissions`).
    pub async fn permissions(&self) -> BackendResult<Vec<String>> {
        let resp =
            call(UNARY_TIMEOUT, self.client.get_permissions(GetPermissionsRequest::default()))
                .await?;
        Ok(resp.macaroon.map(|m| m.permissions).unwrap_or_default())
    }

    /// Creates a macaroon and returns its hex token.
    pub async fn create_macaroon(&self, name: &str, permissions: &[&str]) -> BackendResult<String> {
        let req = CreateMacaroonRequest {
            name: name.to_string(),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        };
        Ok(call(UNARY_TIMEOUT, self.client.create_macaroon(req)).await?.token)
    }

    /// Counts channels (used by health checks).
    pub async fn channel_counts(&self) -> BackendResult<crate::ChannelCounts> {
        let ch = LightningBackend::list_channels(self).await?;
        Ok(crate::ChannelCounts {
            total: ch.len(),
            usable: ch.iter().filter(|c| c.is_usable).count(),
        })
    }
}

fn convert_channel(c: t::Channel) -> Channel {
    let cfg = c.channel_config.clone().unwrap_or_default();
    Channel {
        channel_id: c.channel_id,
        user_channel_id: c.user_channel_id,
        counterparty_node_id: c.counterparty_node_id,
        short_channel_id: c.short_channel_id,
        funding_txid: c.funding_txo.map(|o| o.txid),
        capacity_sat: c.channel_value_sats,
        outbound_capacity_msat: c.outbound_capacity_msat,
        inbound_capacity_msat: c.inbound_capacity_msat,
        is_outbound: c.is_outbound,
        is_channel_ready: c.is_channel_ready,
        is_usable: c.is_usable,
        is_announced: c.is_announced,
        confirmations: c.confirmations,
        confirmations_required: c.confirmations_required,
        fee_base_msat: cfg.forwarding_fee_base_msat,
        fee_ppm: cfg.forwarding_fee_proportional_millionths,
        cltv_expiry_delta: cfg.cltv_expiry_delta,
        force_close_spend_delay: c.force_close_spend_delay,
    }
}

fn lightning_balance(b: t::LightningBalance) -> Option<BalanceItem> {
    use t::lightning_balance::BalanceType as B;
    let (kind, channel_id, amount) = match b.balance_type? {
        B::ClaimableOnChannelClose(x) => {
            ("claimable_on_channel_close", x.channel_id, x.amount_satoshis)
        }
        B::ClaimableAwaitingConfirmations(x) => {
            ("claimable_awaiting_confirmations", x.channel_id, x.amount_satoshis)
        }
        B::ContentiousClaimable(x) => ("contentious_claimable", x.channel_id, x.amount_satoshis),
        B::MaybeTimeoutClaimableHtlc(x) => {
            ("maybe_timeout_claimable_htlc", x.channel_id, x.amount_satoshis)
        }
        B::MaybePreimageClaimableHtlc(x) => {
            ("maybe_preimage_claimable_htlc", x.channel_id, x.amount_satoshis)
        }
        B::CounterpartyRevokedOutputClaimable(x) => {
            ("counterparty_revoked_output_claimable", x.channel_id, x.amount_satoshis)
        }
    };
    Some(BalanceItem { kind: kind.into(), channel_id: Some(channel_id), amount_sat: amount })
}

fn sweep_balance(b: t::PendingSweepBalance) -> Option<BalanceItem> {
    use t::pending_sweep_balance::BalanceType as B;
    let (kind, channel_id, amount) = match b.balance_type? {
        B::PendingBroadcast(x) => ("pending_broadcast", x.channel_id, x.amount_satoshis),
        B::BroadcastAwaitingConfirmation(x) => {
            ("broadcast_awaiting_confirmation", x.channel_id, x.amount_satoshis)
        }
        B::AwaitingThresholdConfirmations(x) => {
            ("awaiting_threshold_confirmations", x.channel_id, x.amount_satoshis)
        }
    };
    Some(BalanceItem { kind: kind.into(), channel_id, amount_sat: amount })
}

pub(crate) fn convert_payment(p: t::Payment) -> Option<LnPayment> {
    use t::payment_kind::Kind as K;
    let kind = match p.kind.as_ref()?.kind.clone()? {
        K::Onchain(o) => LnPaymentKind::Onchain {
            txid: o.txid,
            confirmed_height: match o.status.and_then(|s| s.status) {
                Some(t::confirmation_status::Status::Confirmed(c)) => Some(c.height),
                _ => None,
            },
        },
        K::Bolt11(b) => LnPaymentKind::Bolt11 { payment_hash: b.hash, preimage: b.preimage },
        K::Bolt12Offer(b) => LnPaymentKind::Bolt12Offer {
            payment_hash: b.hash,
            preimage: b.preimage,
            offer_id: b.offer_id,
            payer_note: b.payer_note,
            quantity: b.quantity,
        },
        K::Bolt12Refund(b) => {
            LnPaymentKind::Bolt12Refund { payment_hash: b.hash, preimage: b.preimage }
        }
        K::Spontaneous(s) => {
            LnPaymentKind::Spontaneous { payment_hash: s.hash, preimage: s.preimage }
        }
    };
    Some(LnPayment {
        id: p.payment_id.clone(),
        kind,
        direction: if p.direction() == t::PaymentDirection::Inbound {
            Direction::Inbound
        } else {
            Direction::Outbound
        },
        status: match p.status() {
            t::PaymentStatus::Pending => LnStatus::Pending,
            t::PaymentStatus::Succeeded => LnStatus::Succeeded,
            t::PaymentStatus::Failed => LnStatus::Failed,
        },
        amount_msat: p.amount_msat,
        fee_msat: p.fee_paid_msat,
        updated_at: p.latest_update_timestamp,
    })
}

fn convert_event(env: EventEnvelope) -> Option<LnEvent> {
    use event_envelope::Event as E;
    Some(match env.event? {
        E::PaymentReceived(e) => LnEvent::PaymentReceived { payment: convert_payment(e.payment?)? },
        E::PaymentSuccessful(e) => LnEvent::PaymentSuccessful {
            payment: convert_payment(e.payment?)?,
            preimage: e.payment_preimage,
            bolt12_invoice: e.bolt12_invoice,
        },
        E::PaymentFailed(e) => LnEvent::PaymentFailed {
            reason: e.reason.map(|_| format!("{:?}", e.reason())),
            payment: convert_payment(e.payment?)?,
        },
        E::PaymentClaimable(e) => LnEvent::PaymentClaimable {
            claim_deadline: e.claim_deadline,
            claimable_amount_msat: e.claimable_amount_msat,
            payment: convert_payment(e.payment?)?,
        },
        E::PaymentForwarded(_) => LnEvent::PaymentForwarded,
        E::ChannelStateChanged(e) => LnEvent::ChannelStateChanged {
            state: format!("{:?}", e.state()),
            reason: e.reason.map(|r| r.message).filter(|m| !m.trim().is_empty()),
            channel_id: e.channel_id,
            counterparty_node_id: e.counterparty_node_id,
        },
        _ => return None,
    })
}

fn description(d: InvoiceDescription) -> t::Bolt11InvoiceDescription {
    use t::bolt11_invoice_description::Kind;
    t::Bolt11InvoiceDescription {
        kind: Some(match d {
            InvoiceDescription::Direct(s) => Kind::Direct(s),
            InvoiceDescription::Hash(h) => Kind::Hash(h),
        }),
    }
}

fn route_params(max_fee_msat: Option<u64>) -> Option<t::RouteParametersConfig> {
    max_fee_msat.map(|fee| t::RouteParametersConfig {
        max_total_routing_fee_msat: Some(fee),
        max_total_cltv_expiry_delta: ldk_server_client::DEFAULT_MAX_TOTAL_CLTV_EXPIRY_DELTA,
        max_path_count: ldk_server_client::DEFAULT_MAX_PATH_COUNT,
        max_channel_saturation_power_of_half:
            ldk_server_client::DEFAULT_MAX_CHANNEL_SATURATION_POWER_OF_HALF,
    })
}

#[async_trait]
impl LightningBackend for LdkServerBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            bolt12: true,
            hold_invoices: true,
            splicing: true,
            force_close: true,
            jit_receive: self.lsps2_lsp.is_some(),
            payer_proof: true,
            offer_issuer: false,
            single_use_offers: false,
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<LnEvent> {
        self.start_events();
        self.events.subscribe()
    }

    async fn node_info(&self) -> BackendResult<NodeInfo> {
        LdkServerBackend::node_info(self).await
    }

    async fn graph_stats(&self) -> BackendResult<GraphStats> {
        let (channels, nodes) = tokio::try_join!(
            call(
                UNARY_TIMEOUT,
                self.client.graph_list_channels(GraphListChannelsRequest::default())
            ),
            call(UNARY_TIMEOUT, self.client.graph_list_nodes(GraphListNodesRequest::default())),
        )?;
        Ok(GraphStats {
            channels: channels.short_channel_ids.len() as u64,
            nodes: nodes.node_ids.len() as u64,
        })
    }

    async fn balances(&self) -> BackendResult<Balances> {
        let r =
            call(UNARY_TIMEOUT, self.client.get_balances(GetBalancesRequest::default())).await?;
        Ok(Balances {
            onchain_total_sat: r.total_onchain_balance_sats,
            onchain_spendable_sat: r.spendable_onchain_balance_sats,
            anchor_reserve_sat: r.total_anchor_channels_reserve_sats,
            lightning_total_sat: r.total_lightning_balance_sats,
            lightning: r.lightning_balances.into_iter().filter_map(lightning_balance).collect(),
            pending_sweeps: r
                .pending_balances_from_channel_closures
                .into_iter()
                .filter_map(sweep_balance)
                .collect(),
        })
    }

    async fn list_channels(&self) -> BackendResult<Vec<Channel>> {
        let r =
            call(UNARY_TIMEOUT, self.client.list_channels(ListChannelsRequest::default())).await?;
        Ok(r.channels.into_iter().map(convert_channel).collect())
    }

    async fn open_channel(&self, req: OpenChannelReq) -> BackendResult<String> {
        let amount = match req.amount_sat {
            Some(s) => open_channel_request::Amount::ChannelAmountSats(s),
            None => open_channel_request::Amount::AllFunds(Default::default()),
        };
        let config =
            (req.fee_base_msat.is_some() || req.fee_ppm.is_some()).then(|| t::ChannelConfig {
                forwarding_fee_base_msat: req.fee_base_msat,
                forwarding_fee_proportional_millionths: req.fee_ppm,
                ..Default::default()
            });
        let r = call(
            UNARY_TIMEOUT,
            self.client.open_channel(OpenChannelRequest {
                node_pubkey: req.node_pubkey,
                address: req.address,
                amount: Some(amount),
                push_to_counterparty_msat: req.push_msat,
                channel_config: config,
                announce_channel: req.announce,
                disable_counterparty_reserve: false,
            }),
        )
        .await?;
        Ok(r.user_channel_id)
    }

    async fn close_channel(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        force: bool,
        reason: Option<String>,
    ) -> BackendResult<()> {
        if force {
            call(
                UNARY_TIMEOUT,
                self.client.force_close_channel(ForceCloseChannelRequest {
                    user_channel_id: user_channel_id.into(),
                    counterparty_node_id: counterparty.into(),
                    force_close_reason: reason,
                }),
            )
            .await?;
        } else {
            call(
                UNARY_TIMEOUT,
                self.client.close_channel(CloseChannelRequest {
                    user_channel_id: user_channel_id.into(),
                    counterparty_node_id: counterparty.into(),
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn update_channel_fees(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        fees: ChannelFees,
    ) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.update_channel_config(UpdateChannelConfigRequest {
                user_channel_id: user_channel_id.into(),
                counterparty_node_id: counterparty.into(),
                channel_config: Some(t::ChannelConfig {
                    forwarding_fee_base_msat: fees.fee_base_msat,
                    forwarding_fee_proportional_millionths: fees.fee_ppm,
                    cltv_expiry_delta: fees.cltv_expiry_delta,
                    ..Default::default()
                }),
            }),
        )
        .await?;
        Ok(())
    }

    async fn splice_in(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        amount_sat: u64,
    ) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.splice_in(SpliceInRequest {
                user_channel_id: user_channel_id.into(),
                counterparty_node_id: counterparty.into(),
                amount: Some(splice_in_request::Amount::SpliceAmountSats(amount_sat)),
            }),
        )
        .await?;
        Ok(())
    }

    async fn splice_out(
        &self,
        user_channel_id: &str,
        counterparty: &str,
        address: Option<String>,
        amount_sat: u64,
    ) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.splice_out(SpliceOutRequest {
                user_channel_id: user_channel_id.into(),
                counterparty_node_id: counterparty.into(),
                address,
                splice_amount_sats: amount_sat,
            }),
        )
        .await?;
        Ok(())
    }

    async fn list_peers(&self) -> BackendResult<Vec<Peer>> {
        let r = call(UNARY_TIMEOUT, self.client.list_peers(ListPeersRequest::default())).await?;
        Ok(r.peers
            .into_iter()
            .map(|p| Peer {
                node_id: p.node_id,
                address: p.address,
                is_persisted: p.is_persisted,
                is_connected: p.is_connected,
            })
            .collect())
    }

    async fn connect_peer(&self, node_id: &str, address: &str, persist: bool) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.connect_peer(ConnectPeerRequest {
                node_pubkey: node_id.into(),
                address: address.into(),
                persist,
            }),
        )
        .await?;
        Ok(())
    }

    async fn disconnect_peer(&self, node_id: &str) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.disconnect_peer(DisconnectPeerRequest { node_pubkey: node_id.into() }),
        )
        .await?;
        Ok(())
    }

    async fn onchain_receive(&self) -> BackendResult<String> {
        Ok(call(UNARY_TIMEOUT, self.client.onchain_receive(OnchainReceiveRequest::default()))
            .await?
            .address)
    }

    async fn onchain_send(
        &self,
        address: &str,
        amount: OnchainAmount,
        fee_rate_sat_vb: Option<u64>,
    ) -> BackendResult<String> {
        let amount = match amount {
            OnchainAmount::Sats(s) => onchain_send_request::Amount::AmountSats(s),
            OnchainAmount::All => onchain_send_request::Amount::AllFunds(Default::default()),
        };
        let r = call(
            SEND_TIMEOUT,
            self.client.onchain_send(OnchainSendRequest {
                address: address.into(),
                amount: Some(amount),
                fee_rate_sat_per_vb: fee_rate_sat_vb,
            }),
        )
        .await?;
        Ok(r.txid)
    }

    async fn onchain_bump_fee(
        &self,
        payment_id: &str,
        fee_rate_sat_vb: Option<u64>,
    ) -> BackendResult<String> {
        let r = call(
            SEND_TIMEOUT,
            self.client.onchain_bump_fee(OnchainBumpFeeRequest {
                payment_id: payment_id.into(),
                fee_rate_sat_per_vb: fee_rate_sat_vb,
            }),
        )
        .await?;
        Ok(r.txid)
    }

    async fn sign_message(&self, message: &[u8]) -> BackendResult<String> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.sign_message(SignMessageRequest { message: message.to_vec().into() }),
        )
        .await?;
        Ok(r.signature)
    }

    async fn verify_signature(
        &self,
        message: &[u8],
        signature: &str,
        pubkey: &str,
    ) -> BackendResult<bool> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.verify_signature(VerifySignatureRequest {
                message: message.to_vec().into(),
                signature: signature.into(),
                public_key: pubkey.into(),
            }),
        )
        .await?;
        Ok(r.valid)
    }

    async fn list_forwards(
        &self,
        page_token: Option<String>,
    ) -> BackendResult<(Vec<Forward>, Option<String>)> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.list_forwarded_payments(ListForwardedPaymentsRequest { page_token }),
        )
        .await?;
        let items = r
            .forwarded_payments
            .into_iter()
            .map(|f| Forward {
                id: f.id,
                prev_channel_id: f.prev_channel_id,
                next_channel_id: f.next_channel_id,
                inbound_amount_msat: f.inbound_amount_forwarded_msat,
                outbound_amount_msat: f.outbound_amount_forwarded_msat,
                fee_earned_msat: f.total_fee_earned_msat,
                forwarded_at: f.forwarded_at_timestamp,
            })
            .collect();
        Ok((items, r.next_page_token))
    }

    async fn bolt11_receive(
        &self,
        amount_msat: Option<u64>,
        desc: InvoiceDescription,
        expiry_secs: u32,
    ) -> BackendResult<CreatedInvoice> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.bolt11_receive(Bolt11ReceiveRequest {
                amount_msat,
                description: Some(description(desc)),
                expiry_secs,
            }),
        )
        .await?;
        Ok(CreatedInvoice { invoice: r.invoice, payment_hash: r.payment_hash })
    }

    async fn bolt11_receive_jit(
        &self,
        amount_msat: u64,
        desc: InvoiceDescription,
        expiry_secs: u32,
        max_lsp_fee_msat: Option<u64>,
    ) -> BackendResult<CreatedInvoice> {
        let r = call(
            // The LSPS2 exchange (connect, get_info, get_invoice) can take a moment.
            SEND_TIMEOUT,
            self.client.bolt11_receive_via_jit_channel(Bolt11ReceiveViaJitChannelRequest {
                amount_msat,
                description: Some(description(desc)),
                expiry_secs,
                max_total_lsp_fee_limit_msat: max_lsp_fee_msat,
            }),
        )
        .await?;
        // The RPC returns the invoice only; decode it for the payment hash.
        let d = call(
            UNARY_TIMEOUT,
            self.client.decode_invoice(DecodeInvoiceRequest { invoice: r.invoice.clone() }),
        )
        .await?;
        Ok(CreatedInvoice { invoice: r.invoice, payment_hash: d.payment_hash })
    }

    async fn bolt11_receive_for_hash(
        &self,
        amount_msat: Option<u64>,
        desc: InvoiceDescription,
        expiry_secs: u32,
        payment_hash: &str,
    ) -> BackendResult<String> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.bolt11_receive_for_hash(Bolt11ReceiveForHashRequest {
                amount_msat,
                description: Some(description(desc)),
                expiry_secs,
                payment_hash: payment_hash.into(),
            }),
        )
        .await?;
        Ok(r.invoice)
    }

    async fn bolt11_claim_for_id(
        &self,
        payment_id: &str,
        claimable_amount_msat: u64,
        preimage: &str,
    ) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client.bolt11_claim_for_id(Bolt11ClaimForIdRequest {
                payment_id: payment_id.into(),
                claimable_amount_msat: Some(claimable_amount_msat),
                preimage: preimage.into(),
            }),
        )
        .await?;
        Ok(())
    }

    async fn bolt11_fail_for_id(&self, payment_id: &str) -> BackendResult<()> {
        call(
            UNARY_TIMEOUT,
            self.client
                .bolt11_fail_for_id(Bolt11FailForIdRequest { payment_id: payment_id.into() }),
        )
        .await?;
        Ok(())
    }

    async fn bolt11_send(
        &self,
        invoice: &str,
        amount_msat: Option<u64>,
        max_fee_msat: Option<u64>,
    ) -> BackendResult<String> {
        let r = call(
            SEND_TIMEOUT,
            self.client.bolt11_send(Bolt11SendRequest {
                invoice: invoice.into(),
                amount_msat,
                route_parameters: route_params(max_fee_msat),
            }),
        )
        .await?;
        Ok(r.payment_id)
    }

    async fn decode_invoice(&self, invoice: &str) -> BackendResult<DecodedInvoice> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.decode_invoice(DecodeInvoiceRequest { invoice: invoice.into() }),
        )
        .await?;
        Ok(DecodedInvoice {
            payment_hash: r.payment_hash,
            destination: r.destination,
            amount_msat: r.amount_msat,
            description: r.description,
            description_hash: r.description_hash,
            timestamp: r.timestamp,
            expiry_secs: r.expiry,
            network: r.currency,
            is_expired: r.is_expired,
        })
    }

    async fn bolt12_receive(
        &self,
        desc: &str,
        amount_msat: Option<u64>,
        expiry_secs: Option<u32>,
    ) -> BackendResult<CreatedOffer> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.bolt12_receive(Bolt12ReceiveRequest {
                description: desc.into(),
                amount_msat,
                expiry_secs,
                quantity: None,
            }),
        )
        .await?;
        Ok(CreatedOffer { offer: r.offer, offer_id: r.offer_id })
    }

    async fn decode_offer(&self, offer: &str) -> BackendResult<DecodedOffer> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.decode_offer(DecodeOfferRequest { offer: offer.into() }),
        )
        .await?;
        let (amount_msat, currency) = match r.amount.and_then(|a| a.amount) {
            Some(t::offer_amount::Amount::BitcoinAmountMsats(m)) => (Some(m), None),
            Some(t::offer_amount::Amount::CurrencyAmount(c)) => (None, Some(format!("{c:?}"))),
            None => (None, None),
        };
        Ok(DecodedOffer {
            offer_id: r.offer_id,
            description: r.description,
            issuer: r.issuer,
            amount_msat,
            currency,
            absolute_expiry: r.absolute_expiry,
            chains: r.chains,
            is_expired: r.is_expired,
        })
    }

    async fn bolt12_send(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
        max_fee_msat: Option<u64>,
    ) -> BackendResult<String> {
        let r = call(
            SEND_TIMEOUT,
            self.client.bolt12_send(Bolt12SendRequest {
                offer: offer.into(),
                amount_msat,
                quantity: None,
                payer_note,
                route_parameters: route_params(max_fee_msat),
            }),
        )
        .await?;
        Ok(r.payment_id)
    }

    async fn bolt12_payer_proof(
        &self,
        payment_id: &str,
        preimage: &str,
        invoice: &str,
    ) -> BackendResult<String> {
        let r = call(
            UNARY_TIMEOUT,
            self.client.bolt12_create_payer_proof(Bolt12CreatePayerProofRequest {
                payment_id: payment_id.into(),
                payment_preimage: preimage.into(),
                invoice: invoice.into(),
                options: None,
            }),
        )
        .await?;
        Ok(r.payer_proof)
    }

    async fn payment(&self, id: &str) -> BackendResult<Option<LnPayment>> {
        match call(
            UNARY_TIMEOUT,
            self.client.get_payment_details(GetPaymentDetailsRequest { payment_id: id.into() }),
        )
        .await
        {
            Ok(r) => Ok(r.payment.and_then(convert_payment)),
            Err(BackendError::InvalidRequest(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn list_payments(
        &self,
        page_token: Option<String>,
    ) -> BackendResult<(Vec<LnPayment>, Option<String>)> {
        let r = call(UNARY_TIMEOUT, self.client.list_payments(ListPaymentsRequest { page_token }))
            .await?;
        Ok((r.payments.into_iter().filter_map(convert_payment).collect(), r.next_page_token))
    }
}
