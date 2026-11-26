# LSPS2 (bLIP-52) server implementations and public mainnet deployments

Research date: 2026-11-25. GitHub code search hit the API rate limit near the end, so a few repos (nux-wallet, lnurlcash/run, ThirdChildren/LSPS2_JIT_Channel) were not inspected for LSP pubkeys.

## Which LSPS2 server implementations exist and how mature are they?

### Takeaway
Two server implementations are maintained today. One is LDK's `lightning-liquidity` LSPS2 service, exposed through ldk-node and ldk-server. The other is Core Lightning's `experimental-lsps2-service`, which its maintainers plan to remove. Breez lspd shipped an LSPS2 server but has since dropped it, and LND and Eclair have none. Every maintained implementation is labelled experimental, and LDK's had several fund-loss and DoS bugs fixed in Oct–Nov 2026.

### Cited Findings
**LDK (rust-lightning `lightning-liquidity`, ldk-node, ldk-server)**
- ldk-node 0.5.0 added "Initial support for operating as an bLIP-52 / LSPS2 service (#420)", with the note "bLIP-52 / LSPS2 support is considered 'alpha'/'experimental' and should *not* yet be used in production." A later release added "The bLIP-52/LSPS2 service now supports the 'client-trusts-LSP' model. (#687)". — [ldk-node CHANGELOG](https://raw.githubusercontent.com/lightningdevkit/ldk-node/main/CHANGELOG.md)
- LDK #3838 added `client_trusts_lsp` alongside the existing `lsp_trusts_client` model. Under it, the LSP withholds the funding transaction until the receiver reveals the preimage. — [Optech newsletter 2025-10-03](https://bitcoinops.org/en/newsletters/2025/10/03/)
- ldk-server ships an `[liquidity.lsps2_service]` section under the comment "CAUTION: LSPS2 support is highly experimental and for testing purposes only". It exposes `channel_opening_fee_ppm`, `min_channel_opening_fee_msat`, `min/max_payment_size_msat`, `client_trusts_lsp` (default false) and `advertise_service` (whether the service is announced via gossip). It is built with the `experimental-lsps2-support` cargo feature. — [ldk-server contrib config](https://raw.githubusercontent.com/lightningdevkit/ldk-server/main/contrib/ldk-server-config.toml); [toneloc/stable-channels server README](https://github.com/toneloc/stable-channels/blob/main/server/README.md)
- Fund-loss fix, LDK #5042: "Previously, the LSP trusted the forwarding amount specified in the payer's onion payload without verifying the amount of the incoming HTLC. A malicious payer could request a larger outgoing payment while sending less, causing the LSP to cover the difference with its own funds". It shipped in LDK 0.2.7. — [Optech newsletter 2026-10-09](https://bitcoinops.org/en/newsletters/2026/10/09/); Elias Rohrer discussed it as "the third major security item" of those releases in the [Optech podcast recap 2026-10-13](https://github.com/bitcoinops/bitcoinops.github.io/blob/master/_posts/en/podcast/2026-10-13-newsletter-recap.md)
- DoS fix, LDK #5412: repeated duplicate request IDs inflated the pending-request counter. "Reaching the 1,000-request limit caused the LSP to reject requests from other clients". — [Optech newsletter 2026-10-30](https://bitcoinops.org/en/newsletters/2026/10/30/)
- LDK 0.2.8 fixes "DoS vulnerabilities affecting LSPS2 services". — [Optech newsletter 2026-11-13](https://bitcoinops.org/en/newsletters/2026/11/13/)
- A third-party comparison (data as of May 2026) lists LDK `lightning-liquidity` as the only LSPS2 implementation and calls it "currently in alpha". — [Spark LSP comparison](https://www.spark.money/tools/lightning-service-provider-comparison) (secondary source)

**Core Lightning**
- CLN #8569 added "experimental support for JIT channels … in the `lsp-trusts-client` mode and without MPP support", gated behind `experimental-lsps-client` / `experimental-lsps2-service`. It shipped in CLN v25.12. — [Optech newsletter 2025-11-21](https://bitcoinops.org/en/newsletters/2025/11/21/); [Optech 2025-11-28](https://bitcoinops.org/en/newsletters/2025/11/28/); [CLN CHANGELOG](https://github.com/ElementsProject/lightning/blob/master/CHANGELOG.md)
- Recent CLN changelog entries still fix the service, for example "the LSPS2 service now rejects `lsps2.buy` with a `payment_size_msat` outside the offered min/max (#9620)" and "valid_until in the LSPS0 datetime format". — [CLN CHANGELOG](https://github.com/ElementsProject/lightning/blob/master/CHANGELOG.md)
- Issue #9619 (OPEN), "LSPS: ship only the client, remove the LSPS2 service from CLN". The plan removes `experimental-lsps2-service` and `experimental-lsps2-promise-secret`; the code could move to the plugins repo, and a shared `lsps-types` crate would be published. The stated reason: "The service side of LSPS2 is mostly operator code: pricing, liquidity and risk policy, MPP session handling, recovery … most CLN users never run it". Christian Decker agreed: "I hope we can re-add the server-side of the LSPS2, once we have gotten enough of feedback". — [CLN issue #9619](https://github.com/ElementsProject/lightning/issues/9619)

**Breez lspd**
- The Go-module snapshot of breez/lspd from 2025-02-27 contains `lsps0/` and `lsps2/` packages (server.go, intercept_handler.go, fee.go, cleanup_service.go), and `cmd/lspd/run.go` registers `lsps2.RegisterLsps2Server`. The snapshot from 2026-09-06 (`v0.0.0-20260906202033-353a004d73fc`, the latest on proxy.golang.org) has zero `lsps2` references in Go code. Only the `000014_lsps2_buy` DB migration remains. — [proxy.golang.org breez/lspd @latest](https://proxy.golang.org/github.com/breez/lspd/@latest) (I inspected the module zips)
- `github.com/breez/lspd` returned HTTP 404 on the GitHub API on 2026-11-25, so the repo is private, renamed or deleted. An older PR "implement LSPS2 for LND" (#149) shows in search results as open. — [breez/lspd PR #149](https://github.com/breez/lspd/pull/149) (not viewable)
- LQWD runs its own `lqwdtech/lspd`, a non-fork copy of the old Breez lspd with the legacy Breez gRPC flow. Its README has no LSPS2 mentions. — [lqwdtech/lspd](https://github.com/lqwdtech/lspd)

**LND, Eclair, others**
- GitHub issue search for "LSPS2" in lightningnetwork/lnd and ACINQ/eclair returned zero results (2026-11-25). — [gh search, lnd](https://github.com/lightningnetwork/lnd/issues?q=LSPS2); [gh search, eclair](https://github.com/ACINQ/eclair/issues?q=LSPS2)
- Eclair uses its own "on-the-fly funding" with liquidity ads (`from_future_htlc`), not LSPS2. Eclair #3416 fixed a bug that let a peer get on-the-fly funding without paying. — [Optech newsletter 2026-10-30](https://bitcoinops.org/en/newsletters/2026/10/30/). "ACINQ has moved to a different protocol rather than the LSPS specs" — [Spark LSP comparison](https://www.spark.money/tools/lightning-service-provider-comparison)
- Blockstream Greenlight's gl-sdk `receive()` "includes negotiation of an LSPS2 / JIT channel" through a gRPC `lsp_invoice`. The LSP side is Greenlight-internal, not a public node. — [Blockstream/greenlight gl-sdk node.rs](https://github.com/Blockstream/greenlight/blob/main/libs/gl-sdk/src/node.rs)
- SuperScalar (channel factories) has `lsps2.buy`/`get_info` behind `--enable-jit-experimental`: "Unsafe, testing only … Refused on mainnet." — [SuperScalar LSP operator guide](https://github.com/8144225309/SuperScalar/blob/main/docs/lsp-operator-guide.md)
- BitSov ("Bounded LSPS2 funding pilot") serves LSPS2 from a vendored LDK 0.7 `[lightning.lsps2_service]` and states "Mock success is not proof of channel-opening mainnet". — [MindLink-ApS/BitSov docs/LSPS2-LIQUIDITY.md](https://github.com/MindLink-ApS/BitSov-bitsov/blob/main/docs/LSPS2-LIQUIDITY.md)

### Inferences
- In practice, anyone launching a public LSPS2 LSP in late 2026 runs ldk-node, ldk-server, or a custom LDK build. CLN's service is on its way out, Breez's is gone, and LND and Eclair never had one.
- Breez probably dropped lspd's LSPS2 server along with its move away from the Greenlight/lspd SDK, but I have no source for the reason.

### Gaps
- I couldn't read the breez/lspd commit that removed LSPS2, or the repo's current status, because GitHub returns 404.
- I couldn't confirm whether an LND-based LSPS2 server (lspd PR #149 or any other) is deployed anywhere.
- I didn't check whether CLN #9619's removal has merged; the issue was OPEN when read.

## Do the LSPS spec repo and bLIP-52 list implementations or deployments?

### Takeaway
No. The LSPS repo is archived and points to bLIP-52, and neither repo lists implementations or LSP nodes. Discovery is left to bLIP-50/LSPS0 and optional gossip advertisement.

### Cited Findings
- The BitcoinAndLightningLayerSpecs/lsp README lists no implementations, LSPs or pubkeys. Per its archival note (2025-01-10), "LSPS2 has been finalized as bLIP-52", and the repo is read-only. — [BitcoinAndLightningLayerSpecs/lsp](https://github.com/BitcoinAndLightningLayerSpecs/lsp)
- ldk-server: "Supported protocols are discovered per LSP via bLIP-50 / LSPS0 on startup". The service has `advertise_service = false` by default, the flag for "whether the LSPS service will be announced via the gossip network". — [ldk-server contrib config](https://raw.githubusercontent.com/lightningdevkit/ldk-server/main/contrib/ldk-server-config.toml)

### Inferences
- LSPS2 advertising is off by default in ldk-server, so scanning gossip for LSPS feature bits would probably miss most services.

### Gaps
- I didn't fetch blip-0052.md itself. I also didn't scan gossip for nodes advertising the LSPS feature bit.

## Who deploys LSPS2 on mainnet?

### Takeaway
The only public mainnet LSPS2 LSP confirmed by its operator and by independent clients is Megalith LSP 2. Every other lead was LSPS1-only, a test network, unreachable, or an unverified claim. Several ldk-node operators run private mainnet LSPS2 services for their own apps.

### Cited Findings
**Confirmed mainnet**
- Megalith publishes "Our LSPS2 service": `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735`, with "(any token works)". The page now says "running on mainnet and is ready for production" and that a client must "accept the payment within 60 seconds"; it was last updated Nov 22, 2026. — [Megalith LSPS2 docs](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/). Earlier versions (snippet dated March 2026, repeated by a Sept 2026 third-party file) said "we still need to more testing before we can declare it 'production ready'". — [search snippet of same page](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/); [claude-dev-suite SKILL.md](https://github.com/claude-dev-suite/claude-dev-suite/blob/main/skills/bitcoin/lightning/lsp/SKILL.md)
- Megalith's example client repo lists Node ID `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0` with "Token: any string works". The fee is "the larger of 1.4% of the payment and a minimum that depends on on-chain fees … plus 507 sats for each sat/vB … above 1". The smallest invoice is "2,501 sats today for a new node". — [MegalithicBTC/lsps2-client-example](https://github.com/MegalithicBTC/lsps2-client-example)
- Alby's live channel-suggestions API lists one mainnet LSPS2 entry: `identifier: megalithlsps2`, "Megalith LSP 2", pubkey `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0`, min 2,501 / max 16,000,000 sats, `publicChannelsAllowed: false`. Every other mainnet LSP there is LSPS1: Megalith `038a9e56512ec98da2b5789761f7af8f280baf98a09282360cd6ff1381b5e889bf`, Lnserver, Olympus `031b301307574bbe9b9ac7b79cbe1700e31e544513eae0b5d7497483083f99e581`, Magma by Amboss, B HODL. — [getalby.com/api/internal/channel_suggestions](https://getalby.com/api/internal/channel_suggestions) (fetched 2026-11-25). Oddity: Alby's proxy `…/lsp/megalithlsps2/bitcoin/v1/get_info` returns LSPS1-style info whose `uris` point to the LSPS1 node `038a9e…@64.23.162.51:9735`, so don't use that endpoint to find the LSPS2 node. — [Alby proxy get_info](https://getalby.com/api/internal/lsp/megalithlsps2/bitcoin/v1/get_info)
- Independent mainnet clients using Megalith LSPS2:
  - Winnow PR #203 (merged 2026-10-01) makes "Megalith's LSPS2 node ('Megalith Instant')" the default for new mainnet wallets. Its live terms: 3,514 sats or 1.4%, 3,515 sats–16M sats, channel kept ≥13,140 blocks, `client_trusts_lsp: true`. It also states "Olympus doesn't offer LSPS2." — [winnowwallet/winnow PR #203](https://github.com/winnowwallet/winnow/pull/203)
  - zinqq: "First mainnet JIT receive through Megalith" succeeded after a fix. Megalith quotes carry "~10–15 minutes" of `valid_until`, so invoices must be clamped to it. — [ConorOkus/zinqq solution doc](https://github.com/ConorOkus/zinqq/blob/main/docs/solutions/integration-issues/lsps2-jit-invoice-expiry-quote-valid-until-clamp.md)
  - allod PR #5 reports a Megalith failure: it "opened a 0-conf channel, never sent funding_created, dropped the connection ~96s later and failed the payment with temporary_channel_failure". — [frnandu/allod PR #5](https://github.com/frnandu/allod/pull/5) (search-result summary)

**Private / self-run mainnet LSPS2 services (not public)**
- ldk-node issue #1129 documents a mainnet ldk-node LSPS2 service (2026-10-07) with a 400,000-sat announced channel to ACINQ, serving an orange-sdk client. No pubkey is given. — [ldk-node #1129](https://github.com/lightningdevkit/ldk-node/issues/1129)
- toneloc/stable-channels runs LDK Server with `experimental-lsps2-support` as its own LSP; the README describes a signet setup and says mainnet uses "the same shape". — [stable-channels server README](https://github.com/toneloc/stable-channels/blob/main/server/README.md)
- lnurlcash/run says each mint is "kept liquid by LSPS2", with `MINT_LSP_NODE` = "The LSPS2 provider, `pubkey@host:port`". It names no provider. — [lnurlcash/run](https://github.com/lnurlcash/run)

**Claimed but unverified or ruled out**
- LQWD: SaturnZap says mainnet node "LQWD-AI-Grid" `028aebfca56cdc0813ef826f1f0e08adc9a2ce681ec5782c01c22b78e1d6e8eee3` "supports LSPS1 and LSPS2 (JIT channels)" and is the mainnet default. It also lists 18 regional LQWD nodes from lqwd.ai/contracts.json without claiming LSPS2 for them. The caller found 028aebfc…@44.202.235.176:26000 unreachable. — [lqwdtech/SaturnZap lqwd.py](https://github.com/lqwdtech/SaturnZap/blob/main/src/saturnzap/lqwd.py); [SaturnZap configuration.md](https://github.com/lqwdtech/SaturnZap/blob/main/docs/configuration.md)
- OpenBazaar's docs claim "Olympus by ZEUS: LSPS2 at `45.79.192.236:9735` (node ID `031b3013…e581`)" and "Megalith, Breez, and LQwD: each runs LSPS2 service". This conflicts with Winnow ("Olympus doesn't offer LSPS2"), with Alby's API (Olympus is LSPS1 only) and with the caller's own testing (Olympus is Flow + LSPS1). The Breez claim is undercut by lspd having dropped LSPS2. — [OpenBazaar openbazaar-rs docs/lightning.md](https://github.com/OpenBazaar/openbazaar-rs/blob/main/docs/lightning.md)
- ZEUS PR #5765 (OPEN) adds LSPS2 to ZEUS's embedded LDK Node, but "ships dark: no LSPS2 LSP is set by default (`DEFAULT_LSPS2_*` are empty)". This suggests Olympus has no LSPS2 endpoint yet. — [ZeusLN/zeus PR #5765](https://github.com/ZeusLN/zeus/pull/5765)
- A May 2026 comparison names no LSP running LSPS2. Olympus and Alby are LSPS1, and ACINQ uses its own protocol. — [Spark LSP comparison](https://www.spark.money/tools/lightning-service-provider-comparison)

**Test networks (not mainnet)**
- Megalith Mutinynet LSPS2: `03e30fda71887a916ef5548a4d02b06fe04aaa1a8de9e24134ce7f139cf79d7579` (Alby API `megalithlsps2`/signet). OpenBazaar gives the address `@64.23.192.68:9736`, no token, and got a JIT invoice from it over Tor in November 2026. The "C= LSP (`0371d6fd…f5ef2b`) no longer answered". — [Alby channel_suggestions](https://getalby.com/api/internal/channel_suggestions); [OpenBazaar docs/lightning.md](https://github.com/OpenBazaar/openbazaar-rs/blob/main/docs/lightning.md)
- Alby Hub trusts these peers for 0-conf: mainnet Olympus, Megalith (038a9e…), LNServer_Wave `02b4552a7a85274e4da01a7c71ca57407181752e8568b31d51f13c111a2941dce3`, Flashsats `038ba8f67ba8ff5c48764cdd3251c33598d55b203546d08a8f0ec9dcd9f27e3637` and BHODL `03a5c38d0dfd2dd1ebe679c308788c468a0186720bb33973b1c56710ffe6696c08`. Nothing marks any of them as LSPS2. Alby Hub sets an LSPS2 source only through `LDK_LSPS2_ADDRESSES` (`pubkey@host:port`) or the suggestions API. — [getAlby/hub lnclient/ldk/ldk.go](https://github.com/getAlby/hub/blob/master/lnclient/ldk/ldk.go); [getAlby/hub config/models.go](https://github.com/getAlby/hub/blob/master/config/models.go)

### Inferences
- Several independent wallets (Alby Hub, Winnow, zinqq, OpenBazaar's test default) all converge on Megalith, which suggests no second public mainnet LSPS2 LSP exists as of Nov 2026.
- Flashsats, LNServer_Wave and BHODL are worth probing with LSPS0 `list_protocols`, since they already serve LDK clients. That's a lead, not evidence.

### Gaps
- I didn't check LDK Discord, Delving Bitcoin, Stacker News or Nostr directly; my web searches surfaced no LSPS2 launch announcements from them.
- I found no host:port for the LQWD regional nodes and no LSPS2 claim for them.
- I couldn't inspect Nusanian/nux-wallet, lnurlcash/run or ThirdChildren/LSPS2_JIT_Channel for hard-coded LSPs (GitHub rate limit).

## Why is LSPS2 adoption limited?

### Takeaway
There are four main barriers. Running an LSPS2 service is operator-specific business logic, which pushed CLN out and saw Breez drop it. The only maintained server stack is labelled experimental and had fund-loss bugs patched as late as Oct–Nov 2026. LDK has operational traps around MPP collection and HTLC holds. And the large wallet LSPs (ACINQ, Breez, ZEUS/Olympus) built their own JIT protocols before LSPS2 and have kept them.

### Cited Findings
- **Operator policy is the hard part.** "The service side of LSPS2 is mostly operator code: pricing, liquidity and risk policy, MPP session handling, recovery … most CLN users never run it, and it puts LSP business logic on CLN reviewers and CI." Christian Decker added that the interfaces exposed to operators "may have been too narrow or too wide". — [CLN #9619](https://github.com/ElementsProject/lightning/issues/9619)
- **Experimental labels.** ldk-node calls LSPS2 service "alpha/experimental … not yet be used in production", and ldk-server says "highly experimental and for testing purposes only". — [ldk-node CHANGELOG](https://raw.githubusercontent.com/lightningdevkit/ldk-node/main/CHANGELOG.md); [ldk-server config](https://raw.githubusercontent.com/lightningdevkit/ldk-server/main/contrib/ldk-server-config.toml)
- **Recent security bugs in the server.** The LDK #5042 onion-amount fund loss (0.2.7), the #5412 request-counter DoS, and further 0.2.8 LSPS2 DoS fixes all landed in Oct–Nov 2026. — [Optech 2026-10-09](https://bitcoinops.org/en/newsletters/2026/10/09/); [Optech 2026-10-30](https://bitcoinops.org/en/newsletters/2026/10/30/); [Optech 2026-11-13](https://bitcoinops.org/en/newsletters/2026/11/13/)
- **25% HTLC-in-flight cap (ldk-node #1129, OPEN).** "An ldk-node LSP running the LSPS2 service cannot collect a JIT payment larger than 25% of its largest announced inbound channel". rust-lightning's default is `announced_channel_max_inbound_htlc_value_in_flight_percentage: 25`, and ldk-node offers no override. MPP parts "are queued until `expected_payment_size_msat` is reached, with no timer", and ldk-node doesn't expose `fail_intercepted_htlc`. The payer's 100k-sat HTLC was held "13 h 06 m". The only workaround is "over-provisioning every inbound channel to 4× the largest JIT payment, which is a permanent capital cost". — [ldk-node #1129](https://github.com/lightningdevkit/ldk-node/issues/1129). A fix PR, #1206, was CLOSED unmerged; its body says it had "not passed Rust compilation … upstream tests". — [ldk-node PR #1206](https://github.com/lightningdevkit/ldk-node/pull/1206)
- **Trust models.** `lsp_trusts_client` (the LSP broadcasts and risks the client not claiming) was LDK's first and CLN's only mode, and CLN's had no MPP. `client_trusts_lsp` (the LSP withholds the funding transaction until the preimage) came to LDK in #3838 (Oct 2025). ldk-server defaults to `client_trusts_lsp = false`. — [Optech 2025-10-03](https://bitcoinops.org/en/newsletters/2025/10/03/); [Optech 2025-11-21](https://bitcoinops.org/en/newsletters/2025/11/21/); [ldk-server config](https://raw.githubusercontent.com/lightningdevkit/ldk-server/main/contrib/ldk-server-config.toml). CLN's client-hardening plan would "refuse client_trusts_lsp = true unless the user opts in per call or sets an exposure cap". — [CLN #9619](https://github.com/ElementsProject/lightning/issues/9619). Megalith advertises `client_trusts_lsp: true`. — [Winnow PR #203](https://github.com/winnowwallet/winnow/pull/203)
- **Integration pitfalls for clients.** Short quote validity (~10–15 min at Megalith) makes stale invoices fail silently. — [zinqq doc](https://github.com/ConorOkus/zinqq/blob/main/docs/solutions/integration-issues/lsps2-jit-invoice-expiry-quote-valid-until-clamp.md). Megalith's own docs require accepting the payment "within 60 seconds". — [Megalith docs](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/). On LDK 0.2, ZEUS notes, "only channels from the registered LSPS2 LSP accept a single payment up to the full channel size; channels from other LSPs are capped at 10% of it" — [ZEUS PR #5765](https://github.com/ZeusLN/zeus/pull/5765) (search-result summary). ldk-node has one active LSPS2 client source at a time. — [BitSov LSPS2 doc](https://github.com/MindLink-ApS/BitSov-bitsov/blob/main/docs/LSPS2-LIQUIDITY.md)
- **Incumbents use proprietary JIT protocols.**
  - ACINQ uses on-the-fly funding with liquidity ads. — [Optech 2026-10-30](https://bitcoinops.org/en/newsletters/2026/10/30/); [Spark comparison](https://www.spark.money/tools/lightning-service-provider-comparison)
  - ZEUS/Olympus uses Flow 2.0, with LSPS2 off by default in ZEUS. — [ZEUS PR #5765](https://github.com/ZeusLN/zeus/pull/5765)
  - Breez/LQWD lspd use the legacy Breez gRPC `RegisterPayment` flow. — [lqwdtech/lspd README](https://github.com/lqwdtech/lspd)
- **Economics, Megalith as the data point.** The minimum fee is about 2,500–3,514 sats per open, plus 1.4%, with a promised channel lifetime of 13,140 blocks (~3 months) of locked capital. The LSP also bears on-chain fee risk: the fee rises 507 sats per sat/vB. — [Megalith example](https://github.com/MegalithicBTC/lsps2-client-example); [Winnow PR #203](https://github.com/winnowwallet/winnow/pull/203)
- Optech covered a 2026-05-08 "public fraud proof proposal to improve incentives for JIT channels", a sign that the trust/incentive problem is still open. — [Optech JIT channels topic](https://bitcoinops.org/en/topics/jit-channels/)

### Inferences
- An LSP running LSPS2 for anonymous clients takes on unbounded zero-conf capital commitments. Until this autumn's security fixes, it was also exposed to theft through crafted onions. That alone is a strong reason operators have stayed private or used allow-listed tokens. Megalith's "any token works" is an exception.
- LSPS2 buys a channel without an account, which suits clients that can't take a proprietary SDK. Big wallets have their own clients, so they gain little by exposing LSPS2. That leaves the public market to small LDK-centred operators like Megalith.

### Gaps
- I found no source with LSP-side economics (open counts, revenue, capital cost) for any public LSPS2 operator.
- I found no source for the MPP-specific failure modes beyond #1129 and CLN's "without MPP support".
