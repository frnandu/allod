# Mainnet LSPS2 (bLIP-52) LSP directory: operator by operator

Research date: 2026-11-25. Scope: public mainnet LSPS2 services that a third-party LDK Node / ldk-server client could use.
Method: operator docs, Alby's live LSP list (`https://getalby.com/api/internal/channel_suggestions`, fetched 2026-11-25), Alby Hub source (master, commit e9b886a, 2026-11-23), mempool.space Lightning API, a TCP connect probe, Bitcoin Optech, LDK blog, Zeus GitHub.
Tool budget limited this pass, so several operators on the list have no primary-source check. They are listed under Gaps and not ruled in or out.

## Q1: Which operators run a publicly usable mainnet LSPS2 service, and with what node details?

### Takeaway
Only one operator has a documented, open, publicly usable mainnet LSPS2 service: **Megalith LSP 2**, `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735`. Any token works, and its docs (updated 2026-11-22) now call it "ready for production". It is also the only mainnet LSPS2 entry in Alby's live LSP list and the hard-coded LSPS2 default in Alby Hub. Every other operator checked is LSPS1-only, proprietary JIT, unreachable, or has no LSPS2 evidence.

### Cited Findings

**Megalith (Megalithic.me): confirmed documented LSPS2, open**
- The docs give the LSPS2 node as `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735`. They say "any token works", the service "is running on mainnet and is ready for production", and the page was last updated Nov 22, 2026. — [Megalith docs: LSPS2](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/) (same text on the [pages.dev mirror](https://megalithic-docs.pages.dev/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients))
- **Conflict with older copies:** a search-engine snippet of the same URL read "last updated March 7, 2026" and "we still need to more testing before we can declare it 'production ready'". That is the earlier wording, and the live page fetched on 2026-11-25 replaces it with "ready for production". — [Megalith docs (search snippet vs live fetch)](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/). A secondary skill file dated Sept 2026 also repeated the "not production ready" wording. — [claude-dev-suite LSP skill (secondary)](https://github.com/claude-dev-suite/claude-dev-suite/blob/main/skills/bitcoin/lightning/lsp/SKILL.md)
- Fees and limits: the docs page names no opening-fee amount and no min/max channel or payment size. It points to the example README for the fee. The one stated limit is that the client must accept the payment "within 60 seconds". — [Megalith docs: LSPS2](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/)
- The docs recommend LDK Node's `set_liquidity_source_lsps2` and `receive_via_jit_channel` API. — [Megalith docs: LSPS2](https://megalithic-docs.pages.dev/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients)
- Alby's live LSP list (fetched 2026-11-25) has exactly one mainnet `type: LSPS2` entry: "Megalith LSP 2", identifier `megalithlsps2`, `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735`. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)
- Alby Hub's LDK backend takes the first LSPS2 suggestion for the network. If there is none, it falls back to the hard-coded mainnet default `034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735` ("Megalith LSP 2"). It passes no token (`builder.SetLiquiditySourceLsps2(pubkey, address, nil)`). It caps the accepted LSPS2 opening fee at the greater of 5,000,000 msat (5,000 sat) or 10%. It refreshes the LSP's `get_info` fee menu every 60 minutes. — [getAlby/hub lnclient/ldk/ldk.go](https://github.com/getAlby/hub/blob/master/lnclient/ldk/ldk.go)
- mempool.space (queried 2026-11-25) shows the node as alias "Megalith LSP 2" with sockets `64.23.159.177:9735` and `pqt57olhjiiesawpbqwy7wyh2d3bjaxcs7lil5jlh2bq7um77iqovfid.onion:9735`, 9 active channels and 166,000,000 sat (1.66 BTC) capacity. Its node announcement was last updated 2026-08-29. — [mempool.space node API](https://mempool.space/api/v1/lightning/nodes/034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0)
- My own TCP probe of `64.23.159.177:9735` succeeded at 2026-11-25 22:22 UTC. This shows the port is open, not that the LSPS2 handshake works. — local probe, no URL
- Explorer pages for the same node: [1ml Megalith LSP 2](https://1ml.com/node/034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0/statistics), [LN+](https://lightningnetwork.plus/nodes/034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0)
- Megalith's other mainnet node, "Megalith" `038a9e56512ec98da2b5789761f7af8f280baf98a09282360cd6ff1381b5e889bf@64.23.162.51:9735`, is listed as **LSPS1** (not LSPS2). — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)

**Amboss (Magma): LSPS1 only, no LSPS2 evidence**
- "Magma by Amboss" `03d6749842cabfbfbe386b73d2e34294aef89fb816e40e59e42109d830ff5ee89d@44.232.61.181:9819` is in Alby's list as type **LSPS1**. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)

**LNServer (Wave): LSPS1 only, no LSPS2 evidence**
- "Lnserver" `02b4552a7a85274e4da01a7c71ca57407181752e8568b31d51f13c111a2941dce3@178.156.167.202:48049` is in Alby's list as type **LSPS1**. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)
- No search result tied LNServer or Wave to LSPS2. — [search result summary, no primary source]

**B HODL: LSPS1 only (new find)**
- "B HODL ⚡" `03a5c38d0dfd2dd1ebe679c308788c468a0186720bb33973b1c56710ffe6696c08@54.71.27.149:20301` is in Alby's list as type **LSPS1**. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)

**Olympus by ZEUS: LSPS1 (plus Flow), not LSPS2**
- "Olympus by ZEUS" `031b301307574bbe9b9ac7b79cbe1700e31e544513eae0b5d7497483083f99e581@45.79.192.236:9735` is in Alby's list as type **LSPS1**. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)
- Spark's comparison says ZEUS runs Olympus with LSPS1 and LSPS7, and marks LSPS2 "No". — [Spark: LSPS specification progress (secondary)](https://www.spark.money/research/lightning-lsps-specification-progress)
- Zeus PR #5765 "feat(LDK): LSPS2 just-in-time channels" (opened 2026-11-15, ready for review 2026-11-25, not merged) adds an LSPS2 *client* to Zeus's embedded LDK Node. It ships with no default LSPS2 LSP ("`DEFAULT_LSPS2_*` are empty") and keeps Flow 2.0 as the default provider. It was tested against an ldk-server LSPS2 LSP on Mutinynet. The PR does not say Olympus speaks LSPS2. — [ZeusLN/zeus PR #5765](https://github.com/ZeusLN/zeus/pull/5765)

**LQwD: LSPS2 built, but no reachable public node found**
- The LDK blog (2025-01-14) says LQwD built "a specialized LSP Node" using LSPS2/bLIP-52 for JIT channels and invites "collaboration opportunities". It gives no pubkey, host, token or fee schedule. — [LDK blog: LQWD Liquidity Provider](https://lightningdevkit.org/blog/lqwd-liquidity-provider-get-liquidity-when-you-need-it)
- Candidate node "LQwD-Lspd-ldk" `0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705`: mempool.space (2026-11-25) shows socket `192.243.215.101:26000`, **0 active channels, 0 capacity**, last announcement 2026-08-02. — [mempool.space node API](https://mempool.space/api/v1/lightning/nodes/0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705). This conflicts with older figures elsewhere: LN+ shows about 0.903 BTC across 41 channels (updated around June 2026) — [LN+](https://lightningnetwork.plus/nodes/0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705); 1ML shows about 1.32 BTC — [1ML](https://1ml.com/node/0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705/statistics); Lightning Terminal shows about 54.5M sat — [Terminal](https://terminal.lightning.engineering/explore/0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705)
- My TCP probe of `192.243.215.101:26000` failed (timeout) on 2026-11-25, and so did a probe of the known dead LQWD-AI-Grid `44.202.235.176:26000`. — local probe, no URL
- LQWD press releases (April 2026) describe an "AI Launchpad" agent onboarding service "live on Mainnet" and an 18-node network, but do not mention LSPS2. — [finanznachrichten: LQWD AI Launchpad](https://www.finanznachrichten.de/nachrichten-2026-04/68314110-lqwd-technologies-corp-lqwd-introduces-ai-launchpad-to-accelerate-frictionless-lightning-network-onboarding-for-the-machine-economy-296.htm); [Barchart: LQWD AI payment rails](https://www.barchart.com/story/news/3503328/lqwd-builds-the-bitcoin-payment-rails-for-an-economy-where-ai-pays-ai)
- LQwD has an lspd fork. — [github.com/lqwdtech/lspd](https://github.com/lqwdtech/lspd)

**Breez (Breez SDK / lspd): LSPS2 inside a gated SDK, not an open service**
- The Breez SDK needs an API key from Breez ("email us at contact@breez.technology"), and the default LSP is chosen by that key. — [Breez SDK docs: Connecting to an LSP](https://sdk-doc.breez.technology/guide/connecting_lsp.html)
- Spark says "Breez SDK includes LSPS1 and LSPS2 support, with dynamic fee selection according to the LSPS2 standard." — [Spark: LSPS specification progress (secondary)](https://www.spark.money/research/lightning-lsps-specification-progress)
- `git clone https://github.com/breez/lspd.git` returned "repository not found" on 2026-11-25, so the repo has been renamed, made private or removed. I could not check lspd's current LSPS2 code. — local check, no URL
- Breez's 2023 "Open-LSP" programme let third parties run LSPs behind the SDK, with LQwD named as the first. — [Breez Medium: Open-LSP model](https://medium.com/breez-technology/the-breez-open-lsp-model-scaling-lightning-by-sharing-roi-with-3rd-party-lsps-e2ef6e31562e)

**ACINQ / Phoenix: proprietary, not LSPS2**
- Phoenix v2.4.0 (Oct 2024) moved to bLIP-36 (splicing plus liquidity ads). Spark lists no LSPS1 or LSPS2 support. — [Spark: LSPS specification progress (secondary)](https://www.spark.money/research/lightning-lsps-specification-progress)
- ACINQ appears in Alby's list only as an on-chain channel peer: "ACINQ - limited amounts" `03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f`, `3.33.236.230:9735`. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)

**Voltage Flow: shut down**
- Spark says Voltage deprecated Flow 2.0 in September 2025. — [Spark: LSPS specification progress (secondary)](https://www.spark.money/research/lightning-lsps-specification-progress)

**Blockstream: client and library side only**
- Blockstream's cln-lsps supported LSPS0 and LSPS1 and was archived in October 2025 after the code moved into Core Lightning. — [Spark: LSPS specification progress (secondary)](https://www.spark.money/research/lightning-lsps-specification-progress)

**Alby: no LSP of its own (current)**
- Alby Hub's live list names third-party LSPs only (Megalith, LNServer, Olympus, Magma, B HODL). The source has a commented-out `lsp.AlbyPlebsLSP()` reference, which suggests an earlier Alby-run LSP that is no longer active. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions); [getAlby/hub ldk.go](https://github.com/getAlby/hub/blob/master/lnclient/ldk/ldk.go)
- On-chain (non-LSP) peers in the same list: LNBϟG [Hub-1] `034ea80f8b148c750463546bd999bf7321a0e6dfc60aaf84bd0400a2e8d376c0d5`, Kappa (WoS) `0324ba2392e25bff76abd0b1f7e4b53b5f82aa53fddc3419b051b6c801db9e2247`, Megalithic.me [small] `02a98c86ef366ce226aad6e7706959456e1701058915c3cbf527b37da143bb1441`. None is LSPS2. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)

### Inferences
- In practice, Megalith LSP 2 is the only open, documented mainnet LSPS2 endpoint a third-party ldk-server can point at today without signing up: no token, published node URI, and default in Alby Hub, so it carries real traffic.
- The live LSP directory most used by self-hosted wallets (Alby's) had no second mainnet LSPS2 provider as of 2026-11-25. This is strong evidence that none of Magma, LNServer, Olympus or B HODL serves LSPS2 to the public.
- LQwD's LSPS2 node appears to be wound down (0 channels on mempool.space, port unreachable), so it should not be listed as available.
- Breez's LSPS2 is tied to Breez SDK API keys and Breez's or partner LSPs. It is not an open service for outside LDK Node clients.

### Gaps
- No primary-source check this pass for: Hydro and Rails (Amboss), LNBig as an LSP, Lightspark/Spark, Strike, River, Greenlight, c=, Lightning Labs, Bolt, Hydranode, Rizful, Stacker News, Coinos, Swiss Bitcoin Pay, lipa, Flashsats, Boltz, Fedi, Nodeless, Bitrefill/Thor, Kollider, IBEX, Galoy/Blink, Muun. I found no evidence that any of them offers a public LSPS2 service, but I did not search each one individually. Treat them as "not found", not "confirmed absent".
- Flashsats: a secondary source lists it for LSPS1 channel sales with a 1M sat minimum and no LSPS2. — [claude-dev-suite LSP skill (secondary)](https://github.com/claude-dev-suite/claude-dev-suite/blob/main/skills/bitcoin/lightning/lsp/SKILL.md). Not confirmed from Flashsats itself, and Flashsats is absent from Alby's current list.
- Megalith LSP 2 opening fee (min fee, ppm), min/max payment size and channel size: not on the docs page. A client would have to call LSPS2 `get_info` to see them.
- Whether Megalith LSP 2 still answers `lsps2.get_info`: only a TCP connect was checked.

## Q2: Did any LSPs announce LSPS2 support in 2025-2026?

### Takeaway
No major LSP made a public "LSPS2 launch" announcement in 2025-2026. Two things were announced: LQwD's LSPS2 node (LDK blog, Jan 2025), which now looks inactive, and Megalith's quiet upgrade of its docs to "ready for production" (Nov 2026). The rest of the 2025-2026 news is implementation work (LDK, Core Lightning) and client adoption (Alby Hub, Zeus PR).

### Cited Findings
- LSPS2 was finalized as bLIP-52 around 2025-01-10, when the LSP spec repo moved to the bLIP process. — [BitcoinAndLightningLayerSpecs/lsp](https://github.com/BitcoinAndLightningLayerSpecs/lsp)
- ldk-node 0.5.0 (2025-04-29) added an experimental LSPS2 service. ldk-node 0.7.0 (2025-12-03) added the client-trusts-LSP model. — [ldk-node releases](https://github.com/lightningdevkit/ldk-node/releases)
- The lightning-liquidity crate still calls service-side support "beta". — [lightning-liquidity docs](https://docs.rs/lightning-liquidity)
- Core Lightning added experimental LSPS2 (no-MPP, LSP-trusts-client mode) in PR #8569 (Sept 2025). Optech #381 (2025-11-21) covered it. The CLN option is `experimental-lsps2-service`. — [CLN PR #8569](https://github.com/ElementsProject/lightning/pull/8569); [Optech #381](https://bitcoinops.org/en/newsletters/2025/11/21/); [CLN lightningd-config](https://docs.corelightning.org/reference/lightningd-config)
- LSPS2 security fixes in LDK during 2026:
  - LDK #5042 makes an LSP reject a JIT HTLC whose outgoing amount is larger than the incoming one (fund-loss bug). It shipped in security release v0.2.7. — [Optech #426 (2026-10-09)](https://bitcoinops.org/en/newsletters/2026/10/09/)
  - LDK #5412 rejects duplicate pending request IDs, which a client could use to fill the server's request limit (DoS). — [Optech #429 (2026-10-30)](https://bitcoinops.org/en/newsletters/2026/10/30/)
  - LDK v0.2.8 fixed further LSPS2 DoS bugs. — [Optech #431 (2026-11-13)](https://bitcoinops.org/en/newsletters/2026/11/13/)
- ldk-node issue #1129 (around 2026-10-07): an ldk-node LSPS2 LSP cannot collect a JIT payment larger than 25% of its largest announced inbound channel (HTLC-value-in-flight cap), and the payer's HTLC is held. — [ldk-node #1129](https://github.com/lightningdevkit/ldk-node/issues/1129)
- LQwD's LSPS2 LSP node was announced on the LDK blog on 2025-01-14. — [LDK blog](https://lightningdevkit.org/blog/lqwd-liquidity-provider-get-liquidity-when-you-need-it)
- Megalith's LSPS2 page went from "need more testing" (snippet: last updated 2026-03-07) to "ready for production" (last updated 2026-11-22). — [Megalith docs](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/)
- Zeus added an LSPS2 client in PR #5765 (Nov 2026, open, no default LSP). — [Zeus PR #5765](https://github.com/ZeusLN/zeus/pull/5765)

### Inferences
- Since the LSPS2 server code in LDK only recently received fund-safety and DoS fixes (v0.2.7, v0.2.8), any LDK-based LSPS2 operator, Megalith included presumably, should run at least LDK 0.2.8. This is worth noting as a trust consideration. Megalith's software version is not published.
- The ldk-node #1129 cap could limit the largest JIT payment Megalith LSP 2 can forward, if Megalith runs ldk-node as the LSP. Its implementation is not documented.

### Gaps
- I did not search Stacker News, Nostr long-form posts or X for operator announcements in this pass.
- I did not check the LDK blog index for 2025-2026 posts after the LQWD one.

## Q3: Which require a token/API key or sign-up, and which are open?

### Takeaway
Megalith LSP 2 is open ("any token works") and needs no sign-up. Breez's LSPS2 path needs a Breez SDK API key. LQwD offered contact-based "collaboration" only. The other operators found (Magma, LNServer, Olympus, B HODL, Megalith LSP 1) are LSPS1 and out of scope for JIT.

### Cited Findings
- Megalith LSP 2: "any token works". Alby Hub passes a `nil` token. — [Megalith docs](https://docs.megalithic.me/lightning-services/lsps2-get-a-just-in-time-channel-for-mobile-clients/); [getAlby/hub ldk.go](https://github.com/getAlby/hub/blob/master/lnclient/ldk/ldk.go)
- Breez SDK: an API key from Breez is required. — [Breez SDK docs](https://sdk-doc.breez.technology/guide/connecting_lsp.html)
- LQwD: no token terms published. The blog invites contact through lqwdtech.com. — [LDK blog](https://lightningdevkit.org/blog/lqwd-liquidity-provider-get-liquidity-when-you-need-it)
- In the LSPS2 spec, the token is optional and LSP-defined: an LSP may use it to give different offers to different clients. — [LSPS2 spec README](https://github.com/BitcoinAndLightningLayerSpecs/lsp/blob/main/LSPS2/README.md)

### Inferences
- An ldk-server deployment can set `LDK_LSPS2_LSP=034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735` with an empty or arbitrary token. Megalith is the only verified zero-sign-up option.

### Gaps
- Token and sign-up terms for operators not checked this pass (see the Q1 gaps).

## Test-network LSPS2 services (noted separately)
- Megalith (Mutinynet / signet): `03e30fda71887a916ef5548a4d02b06fe04aaa1a8de9e24134ce7f139cf79d7579@64.23.192.68:9736`. Alby's list has it as both LSPS1 and LSPS2 (`megalithlsps2`), and it is Alby Hub's hard-coded signet LSPS2 default. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions); [getAlby/hub ldk.go](https://github.com/getAlby/hub/blob/master/lnclient/ldk/ldk.go). mempool.space's signet API did not know the node, which is expected because Mutinynet is a separate signet.
- Olympus Mutinynet `032ae843e4d7d177f151d021ac8044b0636ec72b1ce3ffcde5c04748db2517ab03@45.79.201.241:9735` is listed only as an on-chain peer, not LSPS2. — [Alby channel_suggestions API](https://getalby.com/api/internal/channel_suggestions)
