# Allod — Implementation Specification

> *Allodium*: land held in absolute ownership, owing no lord.

Status: v1.0 draft, 2026-10-27. Normative for every session or agent that implements Allod.
Keywords MUST, MUST NOT, SHOULD, MAY follow RFC 2119.

---

## 0. How to use this document

- This file is the source of truth. If code and spec disagree, fix the code or open a spec change in this file. Do not drift silently.
- Work proceeds by milestones (§17). Each milestone ends with its acceptance tests passing (`cargo test --workspace`, `pnpm -C web test`, and the listed e2e tests).
- Every section names the reference material to read before implementing it. Reference trees on this machine:

| Ref | Path | What it is |
|---|---|---|
| HUB | `~/workspace/getAlby/hub` (branch `feat/bolt12-nwc-ldk-server-backend`) | Alby Hub (Go + React). Feature-parity reference for NWC, sub-wallets, permissions, frontend UX. |
| HUB-LDKS | `~/workspace/getAlby/hub/lnclient/ldk-server/` | Hub's ldk-server client in Go: event handling, BOLT12 lifecycle, hold invoices, macaroon binding. |
| HUB-NOTES | `~/workspace/getAlby/hub/zz-notes-for-alby.txt` | Known hold-invoice fund-loss bugs in Hub. Allod MUST NOT repeat them. |
| LDKS | `~/workspace/ldk-server` (tracks `lightningdevkit/ldk-server` main, commit `590cdd99`) | ldk-server. Read `docs/api-guide.md`, `docs/request-binding.md`, `ldk-server-grpc/src/proto/*.proto`. |
| LDKS-CLIENT | `~/workspace/ldk-server/ldk-server-client` | Rust client crate. Allod uses it directly. |
| NWC-RS | `~/workspace/nostr-wallet-connect-ldk-server` | Small Rust NWC↔ldk-server bridge. Reference for crate choices (`nostr-sdk` 0.44, `bitcoin-payment-instructions`). |
| NIP47 | `~/workspace/nips/47.md` | NWC core. |
| NWC | `~/workspace/nwc/*.md` (commit `26c1830`) | NWC extensions: 02 notifications, 03 hold invoices, 05 list_transactions, 06 metadata, 07 deep links, 08 client-initiated connections, 09 lookup_payment, 12 BOLT12 offers, 321 BIP-321 pay/receive, XX-onchain, XX-silent-payment. |

When this spec quotes an NWC field shape and the NWC file disagrees, the NWC file wins. Update this spec to match.

---

## 1. Product summary

Allod is a self-custodial Lightning wallet service that runs next to an **ldk-server** node. It provides:

1. **Node management**: channels, peers, on-chain wallet, balances, logs. All of it goes through the ldk-server gRPC API.
2. **A full NWC wallet service**: NIP-47 core plus every NWC extension listed in §8, BOLT12 send and receive included.
3. **NWC connections and accounts**: per-connection permissions, budgets, expiry. **Accounts** (isolated balances, Alby Hub's "sub-wallets") each have their own account in a double-entry ledger on top of one node, with the fixes listed in §6.6.
4. **A REST + SSE API** (`/api/...`), the only interface the web UI and CLI use.
5. **A React web UI**, a single-page app served by the daemon. Its UX follows Alby Hub.
6. **Optional receive identity**: Lightning Address (LUD-16/LNURL-pay, NIP-57 zaps) and BIP-353 records for accounts.

### 1.1 Goals
- Rust only on the backend. One static binary (`allod`) that embeds the web UI.
- Easy to self-deploy as a full node stack: one-command VPS install, Umbrel and StartOS packages, and a generic Docker Compose bundle (§2.3).
- Correct money handling first. No double spend across accounts, no lost funds on crashes, every state transition reconciled against ldk-server.
- Privacy by default. Isolated connections see only their own data.
- No custodial third parties and no account system. Nothing phones home.

### 1.2 Non-goals for v1
- Running LDK in-process. ldk-server is always a separate process.
- Other backends (LND, CLN, ...). Keep the `LightningBackend` trait (§5) clean so more can be added later, but ship only ldk-server.
- Submarine swaps, an app store, fiat on-ramps, Alby-account features, LSP ordering UI (JIT receive is optional in M8).
- Multi-user hosting. One Allod instance = one node owner.
- **Keysend / spontaneous payments**, sending or receiving: no NWC-04 (`pay_keysend`, `multi_pay_keysend`), no API, no UI. Do not implement them, and do not advertise `04` in `extensions`.

### 1.3 License
MIT. Put a `LICENSE` file at the repo root, set `license = "MIT"` in every crate's `Cargo.toml`, and set `"license": "MIT"` in `web/package.json`. Configure `cargo deny` to allow only MIT-compatible dependency licenses.

---

## 2. Architecture

```
              ┌────────────── allod (single binary) ───────────────┐
 Browser ───► │ allod-api (axum)  ── REST /api/*, SSE /api/events  │
 (allod-web)  │      │                                             │
 CLI ───────► │      ▼                                             │
              │ allod-core  (services: Wallet, Apps, Ledger,       │
              │              Permissions, Offers, Keys, Events)    │
              │      │                │                │           │
              │      ▼                ▼                ▼           │
              │ allod-db         allod-ln         allod-nwc        │
              │ (sqlx: SQLite    (LightningBackend (nostr-sdk pool,│
              │  / Postgres)      → ldk-server     NIP-47 handlers,│
              │                   gRPC client)     notifications)  │
              │                                    allod-lnurl     │
              └───────────────────────│────────────────│──────────┘
                                      ▼                ▼
                                 ldk-server       Nostr relays
                                 (gRPC/HTTP2,
                                  TLS, macaroons)
```

### 2.1 Cargo workspace layout

```
allod/
├── Cargo.toml                # [workspace], shared deps, lints
├── SPEC.md                   # this file
├── AGENTS.md                 # short orientation for coding agents → points here
├── crates/
│   ├── allod-types/          # shared domain types, error enum, amounts (Msat newtype)
│   ├── allod-db/             # sqlx pool, migrations (sqlite + postgres), repositories
│   ├── allod-keys/           # BIP39 mnemonic, BIP32 derivation, at-rest encryption
│   ├── allod-ln/             # LightningBackend trait + ldk-server implementation + event stream
│   ├── allod-core/           # services; owns all business rules (ledger, budgets, permissions)
│   ├── allod-nwc/            # NIP-47 transport, request dispatch, method handlers, notifier, info events
│   ├── allod-lnurl/          # LUD-06/16/21 + NIP-57 + BIP-353 helper (feature "lnurl")
│   ├── allod-api/            # axum router, auth (JWT), DTOs, SSE, static file embedding
│   ├── allod/                # binary: daemon (`allod serve`) + CLI subcommands
│   └── allod-devtools/       # dev-only binary: NWC test client, payer helpers, stack checks (§2.3.9)
├── web/                      # React app (pnpm), built into web/dist, embedded via rust-embed
├── tests/e2e/                # e2e tests, run against the regtest overlay (§2.3.9)
├── justfile                  # dev/test commands (§2.3.9)
├── deploy/                   # compose.yaml (production bundle), compose.regtest.yaml overlay, .env.example, Caddyfile, torrc, litestream.yml, install.sh, allod-stack helper, cloud-init (§2.3)
├── images/                   # Dockerfiles: allod, allod-ldk-server (+ ldk-entrypoint.sh), allod-bitcoind, allod-tor, tools
└── packaging/                # startos/ (§2.3.6); the Umbrel app is allod/ at the root
```

Dependency direction: `allod` → `allod-api` → `allod-core` → {`allod-db`, `allod-ln`, `allod-nwc`, `allod-keys`, `allod-lnurl`} → `allod-types`. `allod-nwc` handlers call `allod-core` service traits. Avoid cycles by defining the service traits `allod-nwc` needs in `allod-types` or in `allod-core::ports`, and inject implementations at startup.

### 2.2 Core dependencies (pin exact versions in workspace `Cargo.toml`)

| Purpose | Crate |
|---|---|
| Async runtime | `tokio` (full) |
| HTTP server | `axum` 0.8, `tower-http` (cors, trace, compression), `axum-extra` (cookies) |
| DB | `sqlx` (runtime-tokio, tls-rustls, sqlite, postgres, migrate, chrono/time, json) |
| ldk-server | `ldk-server-client` + `ldk-server-grpc` via git, pinned `rev`. The commit is written in two places only: the `rev` in the workspace `Cargo.toml`, and the `LDK_SERVER_REV` default in `images/ldk-server.Dockerfile` — the image build reads it from there, so Compose, CI and a plain `docker build` need no extra configuration. For local dev use a `[patch]` to `~/workspace/ldk-server`. |
| Nostr | `nostr` 0.45 with features `nip04`, `nip44` (NIP-47 is implemented in `allod-nwc`); `nostr-sdk` 0.45 with `ring` + `rustls-tls-webpki-roots` (TLS for wss relays). Tests use its `local-relay` feature (dev-dependencies only). |
| Lightning parsing | `lightning-invoice` (BOLT11), `lightning` (offers::offer::Offer for local BOLT12 decode, keep its version aligned with ldk-server's LDK), `bitcoin-payment-instructions` (BIP-321 / BIP-353 parsing) |
| Bitcoin | `bitcoin` 0.32 (bip32, secp256k1, network types) |
| Keys | `bip39`, `argon2`, `chacha20poly1305` (XChaCha20-Poly1305), `zeroize`, `rand` |
| Auth | `jsonwebtoken`, `argon2` (password hash) |
| Serde | `serde`, `serde_json`, `serde_with` |
| Errors | `thiserror` (libs), `anyhow` (binary only) |
| Observability | `tracing`, `tracing-subscriber` (json + rolling file via `tracing-appender`) |
| IDs | `ulid` |
| Embedding UI | `rust-embed` |
| CLI | `clap` 4 (derive) |
| Tests | `nostr-relay-builder` (in-process relay), `tempfile`, `insta` (snapshots), `wiremock`, `proptest` |

### 2.3 Deployment and packaging

Allod ships as a **self-deployable full Lightning node stack**: Bitcoin chain access + ldk-server + Allod (API, NWC, web UI), plus optional TLS, Tor and backups. One person should get from nothing to a working node by:
- **VPS / any Linux host**: one installer command, or `docker compose up` with a `.env`.
- **Umbrel**: install from the app store.
- **StartOS (Start9)**: install the `.s9pk`.
- **Other platforms** (RaspiBlitz, myNode, Citadel, Coolify, CasaOS, ...): the generic Compose bundle.

All of these use the **same images and the same Compose service definitions**. Platform packages are thin wrappers that only map platform conventions (data dirs, the Bitcoin node dependency, ports, auth proxy, Tor, backups) onto them.

#### 2.3.1 Components

| Service | Image | Required | Role |
|---|---|---|---|
| `allod` | `ghcr.io/<org>/allod` | yes | API, NWC service, web UI, LNURL. |
| `ldk-server` | `ghcr.io/<org>/allod-ldk-server` | yes | Lightning node. Built and published by the Allod release pipeline at the pinned ldk-server rev (upstream publishes no images). gRPC is only reachable on the internal Docker network. |
| `bitcoind` | `ghcr.io/<org>/allod-bitcoind` (Bitcoin Core, pinned, verified release binaries) | chain mode `bundled` | Pruned by default (`BITCOIN_PRUNE=auto`: sized to the disk, at most `prune=50000`, i.e. 50 GB; settable, `0` = full). `txindex=0`, `blockfilterindex=1` (`BITCOIN_BLOCKFILTERINDEX`), built after the initial sync, and only on unpruned nodes unless the index already exists (see *Initial sync tuning*). RPC internal only. |
| `gossip` | `ghcr.io/<org>/allod-gossip` | `LDK_GOSSIP=rgs` (default) | Local gossip sidecar (§2.3.2 rule 5). Builds the Lightning graph from P2P gossip, checks funding outputs against bitcoind's UTXO set, serves Rapid Gossip Sync snapshots to ldk-server and internal status to Allod. Needs no inbound port. |
| `caddy` | `caddy:2` (pinned) | profile `https` | Public HTTPS with automatic Let's Encrypt for `ALLOD_DOMAIN`, plus on-demand TLS for the public name chosen in Settings (§12.1). Also needed for LNURL/LUD-16, NIP-57 and the NWC-08 HTTP flow. |
| `tor-proxy` | `ghcr.io/<org>/allod-tor` (pinned Tor) | yes | SOCKS proxy for ldk-server outbound to OnionV3 peers. ldk-server sets `[tor].proxy_address = "tor-proxy:9050"` by default; without it ldk-node reports valid onion peer addresses as invalid. No inbound onion services. |
| `tor` | `ghcr.io/<org>/allod-tor` (pinned Tor) | profile `tor` | Optional onion services for the web UI and Lightning P2P port (inbound over Tor). |
| `litestream` | `litestream/litestream` (pinned) | profile `backup` | Continuous replication of `ldk_node_data.sqlite` and `allod.db` (§2.3.7). |
| `postgres` | `postgres:17` (pinned) | profile `postgres` | Alternative storage for Allod and ldk-server (`[storage.postgres]`) for operators who prefer DB-level replication. |

Images: multi-arch (`linux/amd64`, `linux/arm64`), non-root users, read-only root filesystem where possible, `HEALTHCHECK` in every image, published with an SBOM and cosign signatures. Platform manifests reference images **by digest**.

#### 2.3.2 Chain modes (`ALLOD_CHAIN`)

| Mode | Use when | ldk-server config | Notes |
|---|---|---|---|
| `bundled` (default on VPS) | Self-contained node | `[bitcoind]` → `bitcoind:8332` with generated rpcauth | Initial block download takes hours to days; the UI shows sync progress (§2.3.4). Pruned by default; see the pruned-node rules below. |
| `external-bitcoind` (default on StartOS, and on Umbrel when the Bitcoin Node app is installed) | A Bitcoin node already exists | `[bitcoind]` with host, port, user and password from env | Umbrel/StartOS wire this automatically (§2.3.6); on a VPS the installer asks for it. The node may be pruned; Allod detects that through `getblockchaininfo.pruned` and applies the same rules. |
| `electrum` | Small VPS (1 GB RAM) without a full node | `[electrum]` URL of an **external** server | Less private, and gossip cannot be checked against the chain. The UI shows a permanent "light mode" badge. |
| `esplora` | Same as electrum | `[esplora]` URL of an **external** server | Same caveats. |
| `rbitcoin` (experimental, **deferred: not implemented until the owner re-enables it**) | Rust full node instead of Core | `[esplora]` → bundled `rbitcoin` service (`--sh-index --esplora-listen`) | Behind an "experimental" flag until rbitcoin declares its Lightning interface stable (rbitcoin `docs/lightning.md`). Mainnet is blocked unless `ALLOD_ALLOW_EXPERIMENTAL=true`. Pruning: rbitcoin has no Core-style `-prune`. Its `--prune-seqsigwit` drops only historical inputs and witnesses below tip−288, and keeps outputs, txids, spent data and filters, so disk stays in the hundreds of GiB (~730 GiB unpruned). Restoring from the mnemonic with a rescan needs old full transactions, so it needs an unpruned rbitcoin. Not offered as a small-disk option. |

**Chain source in Settings.** `ALLOD_CHAIN` and the `.env` settings are the stack's default. The stack never runs an Electrum or Esplora server (no `electrs` service; the regtest overlay alone adds one for the test matrix). In a managed stack the owner can switch the source in Settings → Bitcoin → Chain source, from any install mode, to: the stack default; an external Bitcoin Core node (`rpc_address` `host:port`, `rpc_user`, `rpc_password`); a public Electrum/Esplora server (mainnet: Alby Esplora `https://electrs.getalbypro.com` and Electrum `ssl://electrum.getalbypro.com:50002`, mempool.space Esplora, Blockstream Esplora and Electrum; signet/testnet4: mempool.space; regtest: none); or their own `ssl://`/`tcp://host:port` (Electrum) or `http(s)://` (Esplora) URL. The UI warns that an external server sees the node's addresses and IP, and that a pruned Bitcoin Core only works while it still has the blocks since the node last read the chain (pruned rules below). Allod writes the choice with a random `id` to `{handoff}/chain-source.json` (mode 0600; it may hold the RPC password, which the API never returns; an empty password on re-save keeps the stored one for the same address and user). ldk-server's entrypoint applies it over the `.env` settings (`bitcoind` → `external-bitcoind`), reports `{mode, kind, url?, rpc_address?, request_id}` in `chain-source.active.json` (no secrets), and watches the request file from the start of the script, so a change also stops a node that is still waiting for an unreachable or syncing bitcoind; the container's restart policy starts it with the new source. Electrum/Esplora with no URL anywhere waits for a choice. API: `GET /api/settings/chain-source` → `{default_mode, network, editable, active, requested, restart_pending, presets[]}` (`restart_pending` until `active.request_id` matches); `PUT /api/settings/chain-source {kind: default|bitcoind|electrum|esplora, url?, rpc_address?, rpc_user?, rpc_password?}` (full owner session; `409` when Allod does not manage ldk-server). `/api/info` reports the effective chain mode, and the stack bitcoind's sync progress and fee estimates are hidden while the node uses another source. Switching is offered on existing nodes (owner decision, see O-10).

**Standby bitcoind (external node gone).** A platform package can ship the bundled pruned bitcoind idle next to an external node (Umbrel with the Bitcoin Node app, §2.3.6), so the node is not stuck without a chain source when that node goes away. The container waits for `{control}/bitcoind-enabled` before starting bitcoind and reports healthy while idle. Allod gets its RPC as `ALLOD_STANDBY_BITCOIND_RPC_ADDRESS` (`host:port`), `ALLOD_STANDBY_BITCOIND_RPC_USER` and `ALLOD_STANDBY_BITCOIND_RPC_PASS` (full access, ldk-server uses it), and needs the control directory; without all four nothing is offered.
- While `ALLOD_CHAIN=external-bitcoind` and the owner has not picked another source (no request, or `default`), Allod calls `getblockchaininfo` on the configured bitcoind every 30 s. When every call has failed for **3 minutes** (`SUGGEST_AFTER_SECS`, long enough for an app restart or update), the UI shows a banner on every page: the node is unreachable since a time, the last error, and **Start built-in node** next to a link to the other chain sources. It says that the Lightning node cannot pay, receive or watch channels until it reads the chain again, and that the built-in node first downloads the blockchain (hours to days, ldk-server waits for its IBD), so a public server is the faster stopgap. Allod never switches on its own.
- **Start built-in node** (also a choice in Settings → Bitcoin → Chain source) writes `bitcoind-enabled` (`{"enabled_at"}`; never removed by Allod, so the node keeps running across restarts) and a chain source request `{kind: "bitcoind", rpc_address, rpc_user, rpc_password, standby: true}`. ldk-server's entrypoint treats it as any Bitcoin Core source. Allod then watches the standby node for sync progress and fee estimates (it counts as the stack's own bitcoind), and offers fast sync for it (§17 "Fast sync"). The gossip sidecar keeps its configured bitcoind, so funding checks fall back to unverified while that node is gone.
- API: `GET /api/settings/chain-source` adds `standby: {enabled, in_use, outage_since, outage_error, suggested} | null`; `POST /api/settings/chain-source/standby` (full owner session; `409` when there is no standby node) starts it and returns the same body.

**Pruned bitcoind rules.** Only the `[bitcoind]` backend works with a pruned node. Electrum and Esplora servers (electrs, Fulcrum, mempool/electrs) need an unpruned node, so `electrum`/`esplora` modes always point at a separate, unpruned server. Sources: ldk-server PR #261 (`docs/pruned-bitcoind.md`, open), ldk-node #818/#884 (`rescan_from_height`, merged), and ldk-node `src/gossip.rs`.
1. **Normal operation is supported.** ldk-server only needs blocks after its last persisted chain tip.
2. **The outage window.** If ldk-server is offline while bitcoind prunes past ldk-server's persisted tip, catch-up fails with `Block not available (pruned data)`, and the node must not run channels until it has caught up. Mitigations:
   - Default `BITCOIN_PRUNE=auto`: on bitcoind's first start the entrypoint (`prune-size.sh`) takes the space bitcoind can use (free space on its volume plus its own files), subtracts what pruning does not shrink (mainnet 24 000 MiB: chainstate ≈12 GB, block filter index ≈10 GB, logs and peers; test networks 4 000 MiB) and 10 % headroom, rounds down to 1 000 MiB and clamps to 5 000–50 000 (≈50 GB, several months of blocks). The result is saved in the data directory (`.prune-auto`) and reused, so it does not shrink as blocks fill the disk; when even the minimum does not fit, it logs a warning. A number sets the target directly. **Minimum `prune=5000`** (≈5 GB ≈ 2 500 blocks at ~2 MB per block including undo data, i.e. ~2 weeks, covering LDK's `MAX_LOCAL_BREAKDOWN_TIMEOUT` of 2016 blocks at average block sizes). The installer and the platform packages refuse lower values.
   - Allod measures retention in **blocks**, not bytes: `retained = tip − pruneheight` from `getblockchaininfo`. It shows the result as "can recover from ~N days offline", and warns when `retained < 2016`, which can happen after a run of large blocks.
   - ldk-server and bitcoind start and stop together. On shutdown, ldk-server stops first.
   - Allod compares ldk-server's `current_best_block` with bitcoind's `pruneheight` and tip. It raises a critical alert (and blocks new payments) when the gap is not catching up. Recovery path in the UI: temporarily point ldk-server at an unpruned source (an archival node, or an Esplora/Electrum server the user picks) until it has caught up, then switch back (O-10).
3. **New nodes** check their wallet against the current tip, so no old blocks are needed.
4. **Restore from mnemonic** needs blocks from the wallet's first transaction onward. ldk-server's `--rescan-from-height` / `wallet_rescan_from_height` only works if that height is ≥ `pruneheight`. In the restore wizard, Allod asks for a birthday height or date (default: the height recorded in the channel-state backup, if one exists), checks it against `pruneheight`, and refuses with clear options when blocks are missing: re-sync bitcoind unpruned, or run the rescan once through an unpruned Esplora/Electrum source (O-10).
5. **Gossip.** Allod defaults to Rapid Gossip Sync from a local sidecar (`LDK_GOSSIP=rgs`, `LDK_RGS_URL` defaulting to `http://gossip:8090/snapshot`). No third-party snapshot server is involved; a public one is used only if the operator sets `LDK_RGS_URL`.
   - **Sidecar (`allod-gossip`).** An LDK peer manager with `P2PGossipSync` and no UTXO lookup (the path ldk-node takes with Electrum/Esplora) keeps `ALLOD_GOSSIP_TARGET_PEERS` (default 6) gossip peers connected (`.onion` ones through `tor-proxy`). Peers are discovered, not configured: LDK requests a full two-week sync only from the first five peers of a process, so with a saved graph the sidecar dials nodes drawn at random from the 40 best-connected nodes of that graph that announce a reachable address; with an empty graph (first start), or when those run out, it asks the BOLT 10 DNS seeds in `ALLOD_GOSSIP_DNS_SEEDS` (default for mainnet: `nodes.lightning.directory`, `nodes.lightning.wiki`; testnet: `test.nodes.lightning.directory`; `none` disables; queried at most every 10 min) for `_nodes._tcp` SRV records, whose target labels encode the node id. A peer that fails to connect is skipped for 10 min. `ALLOD_GOSSIP_PEERS` (`pubkey@host:port`, optional) adds fixed peers that are always kept connected. Seed answers are unauthenticated; a bad answer only wastes a connection, syncs the graph (about 20 s on mainnet for the two-week window), keeps live gossip, prunes stale channels hourly, and saves graph and funding checks under `/data`, so a restart serves at once. The setup screen shows the sidecar's progress (`gossip` in `GET /api/setup/progress`: peers, channels learned, funding-check progress with time left, then channels and capacity served) before the Lightning node starts, and the Lightning node row says it waits for the network graph. Signatures are checked by LDK; funding is checked separately. The sidecar's node key is random, stored in `/data/node_seed`, and holds no funds.
   - **Funding checks.** With `ALLOD_GOSSIP_VERIFY=auto` (default: on when bitcoind answers within two minutes of start; `true`/`false` force it), every channel's expected 2-of-2 P2WSH script is rebuilt from the announcement's bitcoin keys (`make_funding_redeemscript`) and all of them are looked up in one `scantxoutset` pass over the UTXO set, repeated every `ALLOD_GOSSIP_SCAN_INTERVAL_SECS` (default 1800). A scan runs only once bitcoind reports `initialblockdownload: false`; before that the UTXO set is partial and would fail most channels. On a fresh stack the graph therefore syncs from peers while bitcoind does, the first scan starts when the chain reaches the tip, and `/status` carries `verify.waiting_for_chain` (blocks, headers, verification progress) meanwhile; saved funding checks keep being served. This reads the chainstate, so it works on a pruned node and downloads no blocks. A channel passes when an unspent output with that script exists at the short channel id's block height and output index; its amount becomes the channel capacity. The transaction's position in the block is not checked (it would need the block); instead, when several short channel ids match the same funding output (`txid:vout`), all of them fail, so one output can never be served as many channels. Only the holder of the channel's funding keys can sign such copies, so this never drops another party's channel. With checks on, a snapshot contains only passing channels; unchecked ones wait for the next scan, closed or unfunded ones are left out and age out of ldk-node's graph. With checks off (electrum/esplora stacks without bitcoind), every channel is served without capacity, the same trust as ldk-node's own Electrum/Esplora gossip.
   - **Snapshots.** `GET /snapshot/<timestamp>` returns a full RGS v2 snapshot (the format of `lightning-rapid-gossip-sync` at the pinned rust-lightning commit), ignoring the timestamp: channels with an update from the last 14 days, their updates, node features and addresses, and `funding_sats` per channel. `/snapshot` answers `503` until the graph has stopped changing for `ALLOD_GOSSIP_SETTLE_SECS` (default 45 s) and, with checks on, a first scan finished; `200` afterwards. ldk-node fetches once at start and then hourly (not configurable), and ignores announcements for channels it already knows, so a capacity arrives only with a channel's first snapshot. The ldk-server entry point therefore waits up to `LDK_RGS_WAIT_SECS` (default 600) for `/snapshot` to stop answering `503` before starting ldk-server in RGS mode; if it never does, ldk-server starts with P2P gossip.
   - **P2P (`LDK_GOSSIP=p2p`).** ldk-node gossips with its own peers. With a bitcoind source it validates each channel announcement against its funding block and the UTXO set; on a pruned node, announcements whose funding block is pruned fail that check and are dropped, so the graph stays partial. Use the sidecar with a pruned node. With Electrum/Esplora, ldk-node accepts announcements without a chain check. P2P gossip has no fixed denominator, so the UI MUST NOT show a completion percentage or claim 100% sync; `active` means the graph counts are non-zero and have stopped changing.
   - **Status.** The sidecar exposes `/status` (sync state, peers, graph counts, initial sync time, scan progress and results, unspent capacity, last snapshot served) and `/health`. Allod reads `ALLOD_GOSSIP_SIDECAR_URL` and reports `source: "sidecar"` with these numbers at `GET /api/node/gossip`, next to the node's own channel and node counts from ldk-server. In sidecar mode the state is `learning` while the sidecar syncs or scans and `degraded` when it is unreachable or a scan failed.
6. Electrs as an add-on (§2.3.1) is only offered when the bundled bitcoind is unpruned.

**Anchor channel on-chain reserve.** ldk-node keeps a per-anchor-channel on-chain reserve for fee-bumping channel closes, defaulting to 25 000 sats. Allod ships `anchor_per_channel_reserve_sats = 0` (rendered from `LDK_ANCHOR_PER_CHANNEL_RESERVE_SATS`, default `0`) so an inbound anchor channel, including an LSP-funded LSPS1/LSPS2 channel, is accepted with no on-chain balance. This is a deliberate trade-off: if a channel needs an urgent close while the wallet has no on-chain funds, the node may be unable to raise the closing fee until the wallet is funded. Operators may set a higher value, and the UI warns (Node → Overview and Channels) when the spendable on-chain balance is below 25 000 sats per channel, explaining the close-fee risk. This is narrower than `trusted_peers_no_reserve`, which also asks LDK to rely on the peer to confirm closing transactions.

**LSPS2 LSP (just-in-time receive).** The bundle configures an LSPS2 liquidity source in ldk-server's `config.toml` (`[[liquidity.lsps_client]]`: `node_pubkey`, `address`, `token`, `trust_peer_0conf`), rendered by the ldk entrypoint from `LDK_LSPS2_LSP` (`pubkey@host:port`, empty or unset disables), `LDK_LSPS2_LSP_TOKEN` (LSP-issued API token) and `LDK_LSPS2_LSP_TRUST_0CONF` (default `false`: also accept 0-conf channels from the LSP, so JIT payments land without a confirmation — a trust decision). The same address reaches Allod as `ALLOD_LSPS2_LSP`, so it knows JIT receive is available and can offer it in the UI; ldk-server does not expose its liquidity sources over gRPC, so the two settings must be kept in sync by the bundle (one `.env` knob, `LDK_LSPS2_LSP`, feeds both). With an LSP configured, receive flows can ask for a JIT-channel invoice (§7.4): the payer's payment makes the LSP open an inbound channel and arrive over it, and the LSP's opening fee is part of the invoice the payer pays, so the receiver still gets the full requested amount. The regtest overlay sets the variables empty: its nodes cannot reach a mainnet LSP.

#### 2.3.3 Managed mode: one recovery phrase, no manual credentials

When Allod and ldk-server ship together (`ALLOD_MODE=managed`, the default in every bundle), the user never handles certs, macaroons or a second seed:
1. ldk-server's entrypoint (`ldk-entrypoint.sh`) renders `config.toml` from env, then **waits** until `/handoff/keys_mnemonic` exists. `/handoff` is a small shared volume between `allod` and `ldk-server`.
2. The Allod setup wizard offers "Create new node" (Allod generates a 24-word BIP39 mnemonic) or "Restore" (the user enters a mnemonic, a birthday height/date, and optionally a channel-state backup; §2.3.7 and pruned-node rule 4). Allod passes the birthday to the entrypoint as `wallet_rescan_from_height`. Allod writes `/handoff/keys_mnemonic` (0400) and the backup snapshot if one was given.
3. The entrypoint moves the mnemonic into `<storage_dir>/keys_mnemonic` (0400), deletes it from `/handoff`, and starts ldk-server.
4. Once healthy, the entrypoint copies `tls.crt` and `admin.macaroon` into `/handoff`. Allod connects with them, creates the scoped `allod` macaroon (§3.2 permission list), stores it encrypted (§4.3), and deletes `admin.macaroon` from `/handoff`.
5. Allod derives its Nostr keys from the **same mnemonic** (paths in §4.2 are disjoint from LDK's), so the user backs up **one** phrase. In standalone mode (`ALLOD_MODE=standalone`: connecting to an existing ldk-server, §3.2) Allod keeps its own separate mnemonic as before.
6. The UI makes clear that the phrase restores on-chain funds only, and that channel funds also need the channel-state backup (§2.3.7).

The mnemonic is stored by ldk-server in plaintext inside its volume (an ldk-server constraint). Document this and protect the volume (permissions, disk encryption advice for VPS). Open item O-8: encrypted mnemonic support upstream.

#### 2.3.4 First-run and operations UX (all platforms)
- `/api/info` adds `deployment: {mode, platform: "docker"|"umbrel"|"startos"|"other", chain_mode}` and `chain_sync: {state: "syncing"|"synced", progress, headers, blocks}`. In managed mode with `bundled`/`external-bitcoind`, Allod gets this from `getblockchaininfo` using `ALLOD_BITCOIND_RPC_*`, a read-only rpcauth user with a `rpcwhitelist` of `getblockchaininfo,getnetworkinfo,estimatesmartfee` (the last one backs the `fee_estimates` readiness flag, since ldk-server on mainnet refuses to start before fee estimates exist).
- The setup wizard shows "Bitcoin is syncing: 37%" and lets the user finish setup (password, seed backup) during IBD. Wallet screens stay disabled until ldk-server reports synced.
- **Sync progress stays on screen.** A syncing bitcoind holds its chain lock while it connects blocks or flushes its cache, so `getblockchaininfo` can take a minute on small machines. `GET /api/setup/progress` therefore runs one query at a time (180 s timeout) that keeps going after the request stops waiting (8 s), and returns the last good answer with `updated_at` (unix seconds) and, when the latest query failed, `last_error`; `bitcoind.error` appears only if bitcoind never answered. **Height from the log:** RPC is not enough on its own. On a 4-core VPS with a 1 GiB bitcoind, `getblockchaininfo` took 55 s, `getblockcount` 20 s and even `waitfornewblock` up to 60 s, while `UpdateTip` lines kept arriving every few seconds. So the bitcoind image runs `images/bitcoind/sync-status.sh`, which follows `debug.log` and writes `/control/sync-status.json` (`{height, progress, background_height, at}`, at most every 2 s; with fast sync, tips more than 100 below the highest are the background chainstate). Allod reads it (`ALLOD_CONTROL_DIR`) and, when it is newer than the last RPC answer, at most 30 min old and not more than 100 blocks below it, takes `blocks`, `verification_progress` and `updated_at` from it; headers, `initial_block_download` and fees still come from RPC. Progress requests do not wait for RPC while the log is under 30 s old. The UI times its samples by `updated_at` (a repeated answer is not a stall), says "bitcoind busy, figures from N ago" after 30 s, keeps the rate from before a stall instead of restarting the estimate, ignores dips of Bitcoin Core's `verificationprogress` under 0.5% (it shrinks against the clock between blocks), restarts only on a clear drop or a jump far above the trend (a snapshot), and keeps its samples in `localStorage` for 30 min so a reload does not restart the ETA. **The estimate appears at once:** Allod samples bitcoind every 15 s in the background (every 2 min once synced) and returns `history` (answers ≥10 s apart over the last 15 min, `[unix seconds, blocks, verification progress]`) with the progress, so the UI estimates from its first response; without history it estimates from its second reading. A rough number early is preferred over a precise one late; it settles as the 10-min window fills. A slow sync must visibly move: the percentage shows two decimals under 10% (0.01% of the ~40,000 blocks after a snapshot is 4 blocks) and one above, and estimates of 2–7 days include hours ("about 3 days 7 h left"). Wallet routes answer 503 until the Lightning node runs, so the UI does not request accounts or open live events (`/api/events`) before `GET /api/health` is `ok`. The bitcoind image's healthcheck uses `uptime` (no chain lock), not `getblockchaininfo`.
- Settings → Node → "Deployment" shows images and versions, chain mode, backup status, Tor/HTTPS URLs, disk usage of each volume, and an update-available notice (Allod checks a signed `releases.json` from GitHub only if the user opts in; default off, so nothing phones home).
- Inbound Lightning connectivity: the UI shows whether 9735 is reachable (the user triggers the check; it asks an already connected peer, never a third-party service) and the announce address (`ALLOD_PUBLIC_IP` / domain / onion).

#### 2.3.5 VPS: installer and Compose bundle
Release assets per tag (`release.yml` job `bundle`, which runs `scripts/render-bundle.sh`): `allod-stack-<ver>.tar.gz` (contains `compose.yaml` with pinned image digests, `.env.example`, `Caddyfile`, `litestream.yml`), `install.sh` (a copy of `deploy/install.sh`), and `SHA256SUMS` covering both (the appliance images' checksums are `SHA256SUMS-appliance`). Signing `SHA256SUMS` (and a `torrc` and `allod-stack` helper in the bundle) is still open.

`install.sh` (POSIX sh, short and reviewable; the docs show "download, read, verify the checksum, run", plus a `curl | sh` shortcut for convenience). It prepares the stack and prints how to run it; it never starts the stack:
1. Runs as root (installs to `/opt/allod`) or as any user who can use Docker, through the docker group or rootless Docker (installs to `~/allod`; `--dir` overrides both). Supports Debian 12+/Ubuntu 22.04+ (amd64/arm64; other distributions get a warning). Reports RAM and free disk against the sizing guidance below for the chosen setup, and warns when port 9735 (80/443 with a domain) is already in use.
2. Installs Docker Engine + the compose plugin from Docker's apt repo if missing, after asking (through sudo when not root; it then adds the user to the docker group and asks to log in again and rerun). With rootless Docker it warns when ports 80/443 cannot be published and prints how to start the daemon at boot.
3. Downloads the latest release (or `--version <ver>`, or with `--nightly` the `nightly` pre-release that every Nightly run on the default branch replaces with a bundle pinned to its image digests, §2.3.8) from GitHub, or takes `--bundle <file>`, and checks it against `SHA256SUMS`. It refuses a bundle whose pinned images lack the host's architecture (manual Nightly runs can be amd64 only).
4. Asks: network (`bitcoin`/`signet`/`testnet4`), chain mode (with a disk/RAM estimate for each; Bitcoin Core RPC address and credentials for `external-bitcoind`, a server URL defaulting to the network's first preset for `electrum`/`esplora`), unpruned or pruned for `bundled`, node alias, an announce address (empty by default: private node), domain + ACME email (optional; without a domain the UI is reachable only over Tor or an SSH tunnel, never as plain HTTP on a public IP), Tor on/off (default on without a domain), backup target (Litestream replica URL: S3-compatible + keys, SFTP, a directory, or none, with a warning that channel funds are then at risk).
5. Writes `compose.yaml`, `.env.example`, `Caddyfile`, `litestream.yml` and `.env` (0600) to the install directory from `.env.example` with `COMPOSE_PROFILES` set from the answers, generates the bitcoind RPC passwords, lowers `BITCOIN_MEMORY_LIMIT` to `1g` on hosts with less than 3 GiB RAM, and validates the result with `docker compose config`.
6. Prints how to start and check the stack (`docker compose pull`, `up -d`, `ps`, `logs`), the URLs (domain, SSH tunnel, and where to read the onion address), and the firewall ports to open (22, 80, 443, 9735). It does not change the firewall; Docker's `restart: unless-stopped` brings the stack back after a reboot.
7. Run again, it keeps an existing `.env` (settings and passwords) and only replaces the stack files (a changed file is kept as `<file>.bak`) and `ALLOD_VERSION`. `install.sh --non-interactive --env-file my.env` asks nothing: it applies `my.env` over `.env.example`, drops the `bundled` profile for chain modes without the bundled node, and generates missing passwords. Later: `deploy/cloud-init.yaml` and one-click templates for Hetzner, DigitalOcean and Vultr (cloud-init based), Coolify and CapRover (compose based).

`allod-stack` helper (later; installed to `/usr/local/bin`): `status`, `logs [svc]`, `update [--to <ver>]` (verifies signatures, takes a backup first, pulls pinned digests, restarts, rolls back if health checks fail within 5 min), `backup-now`, `restore <snapshot>`, `config` (edit `.env` safely), `uninstall` (keeps volumes unless `--purge` is typed in full).

Exposure: only 80/443 (Caddy), 9735 (Lightning P2P) and optionally Tor are reachable from outside. ldk-server gRPC, bitcoind RPC/P2P (outbound only by default), Postgres and the Allod port stay on the internal network.

Sizing guidance, published in the docs: `electrum`/`esplora` → 1 vCPU / 1 GB / 20 GB. `bundled` pruned → 2 vCPU / 4 GB / 80 GB SSD. `bundled` full → 2 vCPU / 4 GB / 1 TB SSD.

#### 2.3.6 Umbrel and StartOS packages
**Umbrel** (`allod/` at the repo root): `umbrel-app.yml`, `docker-compose.yml` (app_proxy → `allod_allod_1:8029`), `exports.sh` and `hooks/pre-start`. The repo itself is the community app store (`umbrel-app-store.yml` at the root, store id `allod`; users add `https://github.com/frnandu/allod`). umbrelOS only finds `<app-id>/umbrel-app.yml` one level below the root, hence the location.
- **Chain source, chosen once at the first start.** The manifest declares no dependency (umbrelOS has no optional dependencies, and a declared one must be installed first). `exports.sh` checks whether the Bitcoin Node app (`bitcoin`) is installed:
  - Installed: `ALLOD_CHAIN=external-bitcoind` against `10.21.21.8:8332` with the app's RPC user and password (its `.env`), on the network from its `settings.json` (`main` → `bitcoin`). That node may be pruned (the app's "Prune Old Blocks" setting); §2.3.2's pruned-node rules apply.
  - Not installed: `ALLOD_CHAIN=bundled`. The pruned `bitcoind` service runs right away, mainnet, RPC passwords from `derive_entropy`.
  - `hooks/pre-start` saves the choice and network to `data/umbrel-chain.env`; `exports.sh` reuses it on every later start. Installing or removing the Bitcoin Node app later never switches the node's chain source by itself; the owner switches it in Settings → Bitcoin → Chain source.
  - With the Bitcoin Node app, the bundled `bitcoind` still runs as a **standby** (§2.3.2): `ALLOD_STANDBY=1`, the compose file wraps the image's entrypoint to wait for `/control/bitcoind-enabled` and overrides the healthcheck to pass while idle. If the app is removed, Allod suggests starting it. Known limit: if the Bitcoin Node app's data folder was moved (umbrelOS 2.0 storage), its network is not read and defaults to mainnet.
- Services address each other by Umbrel's injected container names (`allod_<service>_1`), because every Umbrel app shares `umbrel_main_network` and short service names can collide. ldk-server's TLS certificate names `allod_ldk-server_1`.
- Umbrel's Tor SOCKS proxy (`TOR_PROXY_IP:TOR_PROXY_PORT`) replaces the stack's `tor-proxy` service. Lightning P2P on **9736** by default, so it doesn't clash with Umbrel's LND app on 9735. Tor for the web UI through Umbrel's hidden-service support.
- Umbrel's login stays on (app_proxy default). Allod also keeps its own Nostr login behind it.
- Volumes are bind mounts under `${APP_DATA_DIR}/data/`. `backupIgnore` drops the bundled node's blocks and the gossip graph.
- Images are pinned `tag@digest` (multi-arch index digests).
- Published first in this repo's community app store, then submitted upstream to `getumbrel/umbrel-apps`. Citadel and other Umbrel forks reuse it.

**StartOS** (`packaging/startos/`): StartOS 0.4 `start-sdk` (TypeScript) package producing `allod_<arch>.s9pk`. `packaging/.startos/config.yaml` marks the packaging workspace (each developer's `build.key.pem` signing key sits next to it, never committed); `packaging/README.md` covers build, install and publishing.
- Four subcontainers in one package (`bitcoind`, `gossip`, `ldk-server`, `allod`), same images and environment as `deploy/compose.yaml`. They share the package's network namespace: Allod reaches gRPC at `localhost:3536` (ldk-server's TLS cert names `localhost`), the sidecar at `127.0.0.1:8090` and the bundled bitcoind at `127.0.0.1:8332`. A `chown` oneshot gives the volume folders to uid 1000 on every start (StartOS mounts volumes root-owned). One volume `main` with `allod/`, `ldk/`, `handoff/`, `gossip/`, `control/`, `bitcoind/`.
- **Chain source, chosen once at the first start** (as on Umbrel), saved in `startos-store.json` with the bundled node's random RPC passwords:
  - The Bitcoin Core service (`bitcoind`, Core 28–31 or Knots, mainnet) is installed: `ALLOD_CHAIN=external-bitcoind`. Allod mounts its volume read-only, reads the RPC cookie and uses it for ldk-server, the gossip sidecar and Allod's own reads; a new cookie restarts the service. The RPC address is that service's `rpc` host on the StartOS bridge. The bundled `bitcoind` runs as a **standby** (§2.3.2): `ALLOD_STANDBY=1`, it waits for `/control/bitcoind-enabled` and its health check passes while idle; Allod gets the `ALLOD_STANDBY_BITCOIND_RPC_*` variables. If the Bitcoin Core service is removed, ldk-server gets a placeholder address, and after 3 minutes Allod offers to start the built-in node.
  - Not installed: `ALLOD_CHAIN=bundled`, the pruned bundled `bitcoind` runs right away.
  - The Bitcoin Core service is an **optional** dependency, enabled only for the `external-bitcoind` choice. Installing or removing it later never switches the source; the owner switches it in Settings → Bitcoin → Chain source.
- Interfaces: Web UI (LAN/Tor/clearnet domain via StartOS), Lightning P2P.
- Health checks: "Network graph" (sidecar), "Lightning node" (ldk-server's own health check; loading until setup and the Bitcoin sync finish), "Web Interface", "Allod API" (`/api/health` is `ok`), "Built-in Bitcoin node" (idle standby, starting or running). With the Bitcoin Core service, its sync progress is that dependency's `sync-progress` check.
- Actions: "Add owner key" (runs `allod owner add`, the recovery path for a lost Nostr key), "Show onion and connection info" (node id, peer URIs, Web UI addresses). A "Testing" group action toggles `ALLOD_SKIP_BACKUP_CHECK` (off by default). The recovery phrase is shown only inside the Allod UI after re-authentication, never through an action.
- Backups use the StartOS backup API, which stops the service first; the `gossip/` and `bitcoind/` folders are left out. On restore, a critical "Confirm restore" task shows the stale-channel-state warning (§2.3.7) and blocks startup until the user confirms.
- Distribution: sideload, or a StartOS registry (the `startos-registry` service or `start-registry`), StartOS's equivalent of a community app store. The registry indexes `.s9pk` URLs (GitHub release assets) and offers the package-signing key to users who add it under System → Registries. Later, submission to the Start9 Community registry.

**Others**: RaspiBlitz/myNode bonus scripts that wrap the Compose bundle (M10). A NixOS module (M10, community welcome).

#### 2.3.7 Backups (channel state is the hard part)
- What must be backed up: the mnemonic (once, on paper), plus ldk-server `ldk_node_data.sqlite` (or its Postgres DB) **continuously**, plus `allod.db` (apps, the account ledger).
- **Restoring old channel state can lose funds**: a node that broadcasts a revoked commitment is penalized. Only the latest state is safe. A restore therefore requires the user to confirm that the old node is permanently stopped. In managed mode Allod also writes a `restore_epoch` marker so that two nodes started from the same backup are detected (each holds a lock file in its volume, and Allod refuses to start ldk-server if the epoch counter in the backup is newer than local).
- Default mechanism: the `litestream` sidecar replicates both SQLite DBs to an S3-compatible bucket, SFTP, or a local path (`sync-interval: 1s`). Encrypt client-side with `age` through a Allod-managed key derived from the mnemonic (`m/1237'/3'/0'`), so restoring with the phrase can also decrypt the backup.
- Postgres mode: document WAL archiving / streaming replication as the operator's job.
- StartOS/Umbrel platform snapshots are only "last resort" backups. Show this in the UI next to the backup status.
- The UI shows backup status (last successful replication, target, size). On mainnet a red banner stays until a backup target is configured or the user explicitly dismisses it ("I understand I can lose channel funds").
- Open item O-9: LDK peer storage (encrypted channel state stored with peers) once ldk-node/ldk-server expose it.

#### 2.3.8 Release pipeline (M0 builds it; later milestones only add to it)
GitHub Actions on tag:
1. Build the web app, then build `allod` for amd64/arm64 (static musl). Build the `allod-ldk-server`, `allod-bitcoind` and `allod-tor` images from pinned sources with verified checksums/signatures.
2. Push multi-arch images to GHCR. Generate SBOMs (`syft`). Sign images (`cosign`, keyless). Each architecture builds on a native runner (no emulation) and the per-arch digests are merged into one manifest. `allod-ldk-server` is also tagged `ldk-<LDK_SERVER_REV>`; a release whose pin already has that tag reuses it instead of rebuilding. Release images use the `release` Cargo profile (LTO, one codegen unit); Rust builds go through `sccache` on the GitHub Actions cache. The nightly workflow (scheduled or manual) builds the same images with the `fast-release` profile, without SBOM/provenance, and tags all four images `nightly-YYYYMMDD-HHMM` (UTC trigger time, usable as `ALLOD_VERSION`; scheduled runs also move `nightly` on the default branch; manual runs build amd64 only, skip the e2e matrix and never move `nightly`); it reuses a release-built `ldk-<rev>` ldk-server but never writes that tag. Release dry runs also skip SBOM/provenance.
3. Render the Compose bundle with image digests filled in, plus `install.sh` and `SHA256SUMS` (`scripts/render-bundle.sh`; signing is open, §2.3.5).
4. Build the Umbrel app folder and the StartOS `.s9pk`, and attach both to the release.
5. Smoke test each artifact on signet in CI: install with `install.sh --non-interactive` in a VM runner, finish setup through the API, check `/api/health`.

#### 2.3.9 Regtest test overlay (for developers and CI)
`deploy/compose.regtest.yaml` is an overlay on the **same** `compose.yaml`: it sets `ALLOD_NETWORK=regtest`, `ALLOD_CHAIN=bundled`, and adds `ldk-bob` + `ldk-carol` (counterparties, Alice↔Bob↔Carol ring), a `miner` (1 block every `MINE_INTERVAL` s), two local relays (`relay`, `relay2`), a one-shot `bootstrap` (funds the nodes, opens channels, idempotent), and `ALLOD_DEV_AUTO_SETUP=true` with `ALLOD_DEV_OWNER_PUBKEY` (regtest only: completes the managed-mode setup with a new recovery phrase and registers a fixed, public test key as owner, whose secret key is in `deploy/regtest.env` for tests). `ALICE_CHAIN` switches the chain mode of the Allod node (`bundled`, `bundled-rest`, `electrum`, `esplora`; `rbitcoin` is deferred) for the integration matrix. `just` recipes: `up`, `down`, `reset`, `mine`, `bob|carol <ldk-server-cli…>`, `nwc-uri`, `nwc`, `e2e`, `e2e-ui`, `e2e-matrix`, `doctor`. The dev-only `allod-devtools` crate provides an NWC test client (with notification listening and the client side of NWC-08) and payer helpers for Bob and Carol. e2e tests (§16.4) run against this overlay, so the production Compose file is exercised by every test run. The overlay uses its own image tag (`ALLOD_VERSION=regtest`) and host ports (UI on `127.0.0.1:18029`; Alice's P2P port is not published), so it never touches a real deployment running on the same machine. In GitHub Actions the `just` recipes also load `deploy/compose.ci.yaml` (`CI_DOCKER_CACHE=1`), which turns on BuildKit's GitHub Actions cache per image so the Rust images are not rebuilt from scratch on every run; the Allod Dockerfile keeps the dependency compilation in its own layer (manifest-only build with stub sources) for the same reason, and builds the workspace's own crates into a BuildKit cache mount on `target/` that a cold builder seeds from that layer — so a rebuild after a source change recompiles only the changed crates and their dependents, like a local incremental `cargo build`. The overlay builds the Allod and tools images with the `fast-release` Cargo profile (`CARGO_PROFILE` build arg; `release` without LTO and with 16 codegen units), so relinking after a change takes seconds; published images keep the default `release` profile.

#### 2.3.10 Appliance images (proof of concept)
`iso/` builds bootable images that run the **same** `compose.yaml` as a VPS install (`iso/README.md`): `allod-amd64.iso` (hybrid BIOS/UEFI, PCs) and `allod-rpi-arm64.img` (Raspberry Pi 4 and 5, SD card or USB). A Debian live system (squashfs root, changes in RAM) boots straight into Docker; the images of every Compose profile are baked in and loaded once, so nothing is pulled at boot. All state (Docker storage with the stack's named volumes, `.env`, Wi-Fi networks) lives in a data location mounted at `/var/lib/allod`. Found at boot in this order: the location chosen in the menu (`allod-data.conf` on the boot medium: `ALLOD_DATA_UUID`, optional `ALLOD_DATA_DIR`), a filesystem labelled `ALLOD_DATA`, or else the largest blank disk (no partition table, no signature, at least 10 GB, not the boot disk), which is formatted. Without any, the stack does not start (channel state never lives in RAM) and the device asks where to keep the data, safest first: a folder `/allod-data` on an existing ext4/xfs/btrfs filesystem with at least 10 GB free (its other files kept), free disk space for a new partition, a blank disk, or an existing partition to format. With a display the kiosk shows the choice on a local page (`allod-setup-ui`, busybox httpd on 127.0.0.1:8030; server-rendered forms without JavaScript, because the kiosk browser holds a page's own requests until user input) and then the web UI; without one, the console menu `allod-choose-data` asks. Both go through `allod-data-api`, which accepts only locations its own scan offers, erasing only with the typed device name, and one choice at a time (lock). The kiosk browser blocks uBlock Origin (it downloaded itself and its lists at every boot and held page requests meanwhile) and locks local HTTPS upgrades and local network access blocking off. The boot medium, swap, LUKS, LVM and RAID members and anything under 10 GB are never offered. A local display shows a splash and then the UI in a kiosk browser (LibreWolf). Its profile lives on the data location (`kiosk/profile`, bind-mounted by `allod-kiosk-profile`; RAM until a location exists, then the kiosk restarts onto it) with shutdown sanitizing off, so a browser-held key (§11.1, NIP-49 encrypted) survives reboots; page console output goes to the journal.
- **Snapshot variants** (`-snapshot`) also carry the mainnet UTXO snapshot (§17 "Fast sync"): in `/snapshot` on the ISO, or on an `ALLOD_SNAP` ext4 partition of the Pi image. The host serves it on the Docker bridge (`http://allod-usb:8088/utxo-<height>.dat`, reachable from containers only) and sets `ALLOD_FASTSYNC_URL` through a generated Compose override, so the setup wizard suggests it instead of the public mirror. bitcoind checks it against its built-in hash as for any download.
- **Networking.** NetworkManager handles Ethernet (DHCP, preferred when connected) and Wi-Fi; saved networks and the Wi-Fi country live on the data disk. Ways to join Wi-Fi without a cable:
  1. **Boot-medium file.** `allod-network.txt` (`WIFI_SSID`, `WIFI_PASSWORD`, `WIFI_COUNTRY`) on the Pi's FAT boot partition, or on the PC image's small FAT partition `ALLODCFG` appended after the ISO data. Applied at boot; once saved, the password is removed from the file.
  2. **Settings → Network** in the web UI and the kiosk (shown only when the host helper answers). Allod has no host network access: the host's `allod-net-api` (root, NetworkManager via `nmcli`, every value a separate argument) listens on `/run/allod-net/api.sock` (mode 0600, uid 1000), mounted into the Allod container and named by `ALLOD_HOST_NETWORK_SOCKET`. One JSON request line per connection, one JSON reply (`ok` plus fields, or `error`). API (full owner session for changes, audit-logged): `GET /api/system/network` → `{available, hostname, country, wifi_device, hotspot, wifi: {ssid, signal}|null, devices: [{device, type, state, connection, ipv4[]}], saved[]}` (`{available: false}` elsewhere); `POST /api/system/network/scan` → `{networks: [{ssid, signal, security, in_use}]}`; `POST /api/system/network/wifi {ssid, password?, hidden?}` (joins and keeps; `422 NETWORK_FAILED` with the reason); `POST /api/system/network/wifi/forget {ssid}`; `PUT /api/system/network/country {country}`. SSIDs are 1–32 bytes, passwords empty (open) or 8–63 printable ASCII characters or 64 hex digits.
  3. **Setup hotspot.** With a Wi-Fi adapter and no default route for 60 s, the host opens an open Wi-Fi `Allod-Setup-XXXX` (last MAC digits) at 10.42.0.1. Its DNS answers every name with 10.42.0.1 and its only web page (busybox httpd, any path, so phones show it as a captive portal) lists the networks scanned before the hotspot opened and joins one. The portal reaches the same helper, limited to `status`, `scan`, `connect` and `country`, never Allod. It closes once a network works, and while open retries saved networks every 5 minutes. Boot with `allod.nohotspot` to disable it.
  4. **Console.** `nmtui` on the root console (tty2) when a keyboard and display are attached.
- **CI.** `.github/workflows/appliance.yml` builds `allod-amd64.iso` and `allod-rpi-arm64.img.xz` from the published stack images of one tag (amd64 on `ubuntu-24.04`, the Pi image natively on `ubuntu-24.04-arm`), with `SHA256SUMS` and a cosign keyless bundle; run by hand for any tag, by Nightly whenever it builds arm64, and by Release (files attached to the release). The installed Debian system is cached for up to a week. Snapshot variants exceed GitHub's 2 GiB release file limit and are built locally.
- Raspberry Pi Imager's "OS customisation" must not be used with these images: it edits `cmdline.txt` to run a Raspberry Pi OS first-boot script that does not exist here. Use `allod-network.txt` instead.
- Not yet: A/B updates and rollback (boot a newer image instead), the migration guard of §2.3.8, disk encryption.

---

## 3. Configuration and lifecycle

### 3.1 Process config (env / CLI flags / `allod.toml`; precedence flags > env > file)

| Key | Env | Default | Notes |
|---|---|---|---|
| `data_dir` | `ALLOD_DATA_DIR` | `~/.allod` | Holds `allod.db`, logs, `jwt.secret`. |
| `database_url` | `ALLOD_DATABASE_URL` | `sqlite://{data_dir}/allod.db` | `postgres://...` supported. |
| `listen` | `ALLOD_LISTEN` | `127.0.0.1:8029` | HTTP API + UI. |
| `public_url` | `ALLOD_PUBLIC_URL` | none | Required for LNURL / NWC-08 HTTP flow. When unset, the public name from Settings is used (§12.1). |
| `tls_check_addr` | `ALLOD_TLS_CHECK_ADDR` | none (compose: `caddy:443`) | Where Allod tests the public name's certificate from inside the stack (§12.1); unset: through public DNS. |
| `dns_resolver` | `ALLOD_DNS_RESOLVER` | `1.1.1.1:53` | DNS server (`ip:port`, TCP) that builds DNSSEC proofs for paying BIP-353 names (§11.3). Allod verifies the proofs itself. |
| `network` | `ALLOD_NETWORK` | taken from ldk-server `GetNodeInfo` | Used only as a guard. On a mismatch with ldk-server, refuse to start. |
| `relays` | `ALLOD_RELAYS` | `wss://relay.getalby.com/v1`, `wss://nos.lol` | Comma-separated. Also editable in settings (DB value wins after setup). |
| `profile_relays` | `ALLOD_PROFILE_RELAYS` | `wss://purplepag.es`, `wss://nos.lol`, `wss://relay.primal.net` | Contact and profile queries only; NWC relays are not added automatically. |
| `log_level` | `ALLOD_LOG` | `info` | `tracing` env-filter syntax. |
| `ldk_server.*` | — | — | Stored encrypted in the DB at setup (§3.2), never in env, so a backup is self-contained. `ALLOD_LDK_SERVER_ADDRESS` / `_CERT_FILE` / `_MACAROON_FILE` MAY override for headless setups. |
| `mode` | `ALLOD_MODE` | `standalone` (binary) / `managed` (bundles) | §2.3.3. |
| `chain` | `ALLOD_CHAIN` | `bundled` | Chain mode written into ldk-server's config by the bundle (§2.3.2). Informational for Allod itself. |
| `handoff_dir` | `ALLOD_HANDOFF_DIR` | `/handoff` | Managed mode credential/seed exchange with ldk-server. |
| `fastsync_url` | `ALLOD_FASTSYNC_URL` | — | UTXO snapshot shipped with this device, suggested for fast sync instead of the public mirror (§2.3.10). |
| `host_network_socket` | `ALLOD_HOST_NETWORK_SOCKET` | — | Unix socket of the appliance host's network helper; enables Settings → Network (§2.3.10). |
| `bitcoind_rpc_*` | `ALLOD_BITCOIND_RPC_URL/_USER/_PASS` | none | Read-only access for sync progress (§2.3.4). |
| `node_explorer_url` | `ALLOD_NODE_EXPLORER_URL` | `https://amboss.space/node/{pubkey}` | Link template for node pubkeys in the UI (peers, channels); `{pubkey}` is replaced; only `http(s)` links are shown; empty disables. Exposed as `deployment.node_explorer_url` in `/api/info`. Aliases never come from the explorer: `GET /api/peers` and `GET /api/channels` add `alias` from ldk-server's `GraphGetNode` (the node's own gossip graph, `graph:read`); RGS snapshots carry no aliases, so nodes ldk-server cannot name are looked up in one batch at the gossip sidecar's `GET /aliases?ids=` (at most 500 ids; aliases from the node announcements it received over P2P; base URL derived from `ALLOD_GOSSIP_SIDECAR_URL`), sanitized (control and invisible formatting characters removed, at most 32 characters), and cached in memory per pubkey (6 h when found, 15 min when unknown, at most 5000 entries). |
| `skip_backup_check` | `ALLOD_SKIP_BACKUP_CHECK` | `false` | Testing only, any network (mainnet included): the setup wizard shows a "Skip check" button on the backup confirm step (§13.3). Exposed as `deployment.skip_backup_check` in `/api/info` (omitted when false); Allod logs a warning at startup when set. The phrase is still shown once; only the re-entry check is skippable. |
| `gossip_sidecar_url` | `ALLOD_GOSSIP_SIDECAR_URL` | none (compose: `http://gossip:8090/status`) | Internal `allod-gossip/status` endpoint; when set, `/api/node/gossip` reports the sidecar as the graph source (§2.3.2 rule 5). Set empty with `LDK_GOSSIP=p2p`. |
| `lsp.lsps2` | `ALLOD_LSPS2_LSP` | none | LSPS2 LSP for JIT receive channels, `pubkey@host:port` (§2.3). Empty disables. Must match ldk-server's liquidity source; Allod uses it to decide whether to offer JIT receive (`Capabilities::jit_receive`). |
| `platform` | `ALLOD_PLATFORM` | `docker` | `docker`/`umbrel`/`startos`/`other`. Selects appliance integration behavior and UI hints. StartOS interface proxy may mint first-run browser claim; NIP-98 remains required for setup. |
| `trusted_setup_proxy` | `ALLOD_TRUSTED_SETUP_PROXY` | none | Hostname of an authenticated appliance reverse proxy allowed to mint the short-lived browser setup claim. Allod verifies the TCP source IP against current DNS results; forwarding headers do not grant access. |
| `dev_auto_setup` | `ALLOD_DEV_AUTO_SETUP` | `false` | Regtest only: runs setup at startup with a new recovery phrase and registers `ALLOD_DEV_OWNER_PUBKEY` as owner (§2.3.9). Allod refuses to start with it set on any other network. |

### 3.2 First-run setup (`POST /api/setup`)
Every setup also registers the **owner**: the Nostr public key that will log in to the web UI (§11.1). NIP-98 proves control of that key but does not authorize it to claim a new node. Before setup, Allod generates a random 256-bit capability in `{data_dir}/secrets/setup_bootstrap` (0600). `allod setup-link` prints `{public_url}/#setup=<capability>`; installers show this as the final, clickable setup action. URL fragment is not sent to Caddy, access logs, or referrers. UI exchanges it through `POST /api/setup/claim` for a one-hour `HttpOnly; SameSite=Strict` cookie (also `Secure` over HTTPS), removes fragment from address bar immediately, then runs normal wizard. `POST /api/setup` requires both claim cookie and NIP-98 signature and binds claim to first signer. Capability is compared in constant time and deleted only after successful setup, so input errors remain retryable. `allod setup-link --rotate` invalidates old links. Bare setup page shows exact host command instead of wizard. An authenticated appliance launcher may mint the same short-lived claim when its TCP source matches `trusted_setup_proxy`; forwarded headers never establish trust. StartOS instead exposes setup links through its launchable **Open setup** action, so owners never locate secret files.

In **managed mode** (§2.3.3) the only other input is either `create: true` or `restore: {mnemonic, birthday_height?, backup?}`. Allod runs the seed handoff and fetches credentials itself; the steps below then apply from step 2. **Standalone mode** inputs:
- `ldk_server_address` (`host:port`)
- `ldk_server_tls_cert_pem`
- `ldk_server_macaroon_hex`
- optional `mnemonic` (restore)

Steps:
1. Refuse if already set up (409), or if browser lacks valid host-issued setup claim (401).
2. Validate the ldk-server connection: `GetNodeInfo`, then `GetPermissions`. Required macaroon permissions: `node:read onchain:receive onchain:send invoices:create payments:read payments:claim payments:send channels:read channels:manage peers:read peers:manage messages:sign utilities:read events:read graph:read`. `channels:force_close` is optional; without it the UI hides force close. Return 400 with the list of missing permissions.
3. Generate a BIP39 24-word mnemonic, or use the provided one. Derive keys (§4).
4. Write the secrets files (§4.3): the recovery phrase and the ldk-server macaroon. Store the ldk-server address and certificate in `config`.
5. Store the owner pubkey in `owners` (§11.1). Mark `setup_completed=true`. Return 201 with a session JWT for the owner (the user just proved the key). The UI then shows the recovery-phrase backup screen (§13).
- Setup is only possible while no owner exists and only through host-issued setup claim. Public HTTPS/Tor exposure before initialization is safe: visitors without claim link can see only locked setup page and cannot register an owner or obtain recovery phrase.

### 3.3 Trust model and startup
Allod is built for hardware the owner controls: a home node, Umbrel or StartOS box, or the owner's own VPS. It does **not** have an unlock step.
- **Why:** in managed mode ldk-server keeps the recovery phrase, its admin credential and the channel state in plain files (an ldk-server constraint, O-8). Anyone who can read the disk can already spend the funds through ldk-server, so encrypting Allod's copy would add no real protection. A VPS provider can read memory too. Meanwhile an unlock step would stop NWC, the reconciler and hold-invoice settlement after every restart until someone typed a secret, and a forgotten secret would lock the owner out of a healthy node.
- **What protects the keys:** file permissions (secrets 0600, data directories 0700, containers as uid 1000), full-disk encryption at the OS level (LUKS) on owned hardware, and choosing a VPS provider the owner is willing to trust with the keys. The setup wizard and the docs say this in plain words.
- **What protects access:** Nostr owner login (§11.1) for the UI and API, and API tokens for scripts.
- **Backups** never contain the recovery phrase: it is not stored in the database, and backups are encrypted with a key derived from it (§2.3.7, §4.3).
- **Startup:** Allod starts all services as soon as setup is complete: LightningBackend (connect + start the event stream) → Reconciler (§7.6) → NWC (connect relays, publish info events, subscribe) → LNURL. Before setup, only `/api/info`, `/api/setup/claim`, `/api/setup`, `/api/auth/*`, `/api/health` and static UI respond; every other route returns 409 `SETUP_REQUIRED`.
- A future, off-by-default "lock on restart" mode may be added for owners who want it. It is not planned for v1.
- `POST /api/stop` shuts services down in reverse order. Shutdown on SIGTERM is graceful: stop accepting NWC requests, wait up to 30 s for in-flight payments to reach a terminal state or be persisted as pending, then exit.

### 3.4 Health
`GET /api/health` returns `{ status: "ok" | "degraded" | "down", checks: { db, ldk_server, event_stream, relays: [{url, connected}], nwc_handler } }`.

---

## 4. Keys and secrets (`allod-keys`)

### 4.1 Material
- **Allod mnemonic** (BIP39, 24 words). Root of every Nostr key and of the backup encryption key. In managed mode it is **the same** mnemonic as ldk-server's `keys_mnemonic` (§2.3.3), so the user has one phrase. In standalone mode it is separate, and the UI MUST tell the user to back up both it and ldk-server's `keys_mnemonic`.
- ldk-server credentials: address, TLS cert, reusable admin-scoped macaroon.

### 4.2 Nostr key derivation
BIP32 from the BIP39 seed (empty passphrase):
- Service key (legacy shared wallet pubkey, for connections created without a dedicated key): `m/1237'/0'/0'`
- Per-app wallet key: `m/1237'/1'/{app_id}'`, where `app_id` is the DB integer id (< 2^31).
Every new connection MUST get its own wallet key, so connections cannot be linked through a shared pubkey.
- Web Push VAPID key material: `m/1237'/4'/0'` (§8.9). The service key exists only for imports and compatibility; new connections never use it.
- Gateway provider account key (LNVPS NIP-98 login): `m/1237'/5'/0'` (§12.8).

### 4.3 Secrets storage and backup encryption
- Secrets live in files under `{data_dir}/secrets/` (directory 0700, files 0600), never in the database: `recovery_phrase` (the 24 words), `ldk_server.macaroon` (the scoped macaroon Allod uses), `clank_token_<name>` and, while a purchase is unfinished, `clank_l402_<target>` (§12.4), `dns_provider_<id>` (DNS provider API tokens, §12.1), `wireguard_home`, `gateway_<id>_wg` and `gateway_ssh` (gateways, §12.7), and pre-setup only `setup_bootstrap` (single-use browser claim capability). Writes are atomic (write to a temp file, fsync, rename).
- In managed mode the recovery phrase file is Allod's own copy; ldk-server keeps `keys_mnemonic` in its volume (§2.3.3).
- The database holds no secrets, so Litestream replicas and exports of it cannot leak keys.
- **Backup and export encryption:** XChaCha20-Poly1305 with a 32-byte key derived from the backup key `m/1237'/3'/0'` (§4.2), values stored as `v1:` + base64(nonce‖ct) (`allod_keys::Vault`). Only someone with the recovery phrase can read a backup.
- The JWT signing key is 32 random bytes generated **in memory at process start** and never stored, so sessions do not survive a restart. API tokens are not JWTs and persist (§11.1).

---

## 5. Lightning backend (`allod-ln`)

### 5.1 Trait

```rust
#[async_trait]
pub trait LightningBackend: Send + Sync + 'static {
    async fn node_info(&self) -> Result<NodeInfo>;              // pubkey, alias, color, network, block height/hash, uris, features, version (`<cargo version> (<commit>)`)
    async fn balances(&self) -> Result<Balances>;
    // BOLT11
    async fn bolt11_receive(&self, req: Bolt11ReceiveReq) -> Result<Bolt11Invoice>;          // amount optional, description | description_hash, expiry
    async fn bolt11_receive_jit(&self, req: JitReceiveReq) -> Result<Bolt11Invoice>;          // LSPS2 just-in-time channel: amount required, optional max_total_lsp_fee_msat; only while an LSP is configured (Capabilities::jit_receive). ldk-server: Bolt11ReceiveViaJitChannel, then DecodeInvoice for the payment hash (the RPC returns only the invoice)
    async fn bolt11_receive_for_hash(&self, req: HoldInvoiceReq) -> Result<Bolt11Invoice>;   // hold invoice
    async fn bolt11_claim_for_id(&self, payment_id: &PaymentId, claimable_msat: u64, preimage: &Preimage) -> Result<()>;
    async fn bolt11_fail_for_id(&self, payment_id: &PaymentId) -> Result<()>;
    async fn bolt11_send(&self, invoice: &str, amount_msat: Option<u64>, max_fee_msat: Option<u64>) -> Result<PaymentId>; // returns immediately
    // BOLT12
    async fn bolt12_receive(&self, req: OfferReq) -> Result<CreatedOffer>;                   // description, amount?, expiry_secs?, quantity?
    async fn decode_offer(&self, offer: &str) -> Result<DecodedOffer>;
    async fn bolt12_send(&self, offer: &str, amount_msat: Option<u64>, quantity: Option<u64>, payer_note: Option<String>, max_fee_msat: Option<u64>) -> Result<PaymentId>;
    async fn bolt12_payer_proof(&self, payment_id: &PaymentId) -> Result<Option<String>>;
    // payments
    async fn payment(&self, id: &PaymentId) -> Result<Option<LnPayment>>;
    async fn list_payments(&self, page_token: Option<String>) -> Result<(Vec<LnPayment>, Option<String>)>;
    fn subscribe(&self) -> broadcast::Receiver<LnEvent>;         // fan-out of the ldk-server event stream
    // on-chain
    async fn onchain_receive(&self) -> Result<Address>;
    async fn onchain_send(&self, req: OnchainSendReq) -> Result<Txid>;  // address, amount_sat | send_all, fee_rate?
    async fn onchain_bump_fee(&self, txid: &Txid, fee_rate: u64) -> Result<Txid>;
    // channels & peers
    async fn list_channels(&self) -> Result<Vec<Channel>>;
    async fn open_channel(&self, req: OpenChannelReq) -> Result<UserChannelId>;
    async fn close_channel(&self, user_channel_id: &str, counterparty: &PublicKey, force: bool, reason: Option<String>) -> Result<()>;
    async fn update_channel_config(&self, req: UpdateChannelReq) -> Result<()>;
    async fn splice_in(&self, req: SpliceInReq) -> Result<()>;
    async fn splice_out(&self, req: SpliceOutReq) -> Result<()>;
    async fn list_peers(&self) -> Result<Vec<Peer>>;
    async fn connect_peer(&self, node_id: &PublicKey, addr: &str, persist: bool) -> Result<()>;
    async fn disconnect_peer(&self, node_id: &PublicKey) -> Result<()>;
    // misc
    async fn sign_message(&self, msg: &[u8]) -> Result<String>;   // zbase32 signature
    async fn verify_signature(&self, msg: &[u8], sig: &str, pubkey: &PublicKey) -> Result<bool>;
    async fn forwarding_stats(&self) -> Result<ForwardingStats>;
    async fn list_forwards(&self, page_token: Option<String>) -> Result<(Vec<Forward>, Option<String>)>;
    async fn graph_node(&self, node_id: &PublicKey) -> Result<Option<GraphNode>>;
    fn capabilities(&self) -> BackendCapabilities;  // bolt12, hold_invoices, splicing, force_close, jit_receive, payer_proof, offer_issuer, single_use_offers
}
```

### 5.2 ldk-server implementation
- Use `ldk_server_client::client::LdkServerClient::new(base_url, macaroon_hex, cert_pem)`. It already binds the macaroon to each request (LDKS `docs/request-binding.md`). Do not reimplement binding.
- Map each trait method to its RPC (`Bolt11Send`, `Bolt12Receive`, `Bolt11ReceiveForHash`, `Bolt11ClaimForId`, `Bolt11FailForId`, `Bolt12Send`, `Bolt12CreatePayerProof`, `GetPaymentDetails`, `ListPayments`, `DecodeOffer`, `OnchainReceive`, `OnchainSend`, `OnchainBumpFee`, `ListChannels`, `OpenChannel`, `CloseChannel`, `ForceCloseChannel`, `UpdateChannelConfig`, `SpliceIn`, `SpliceOut`, `ListPeers`, `ConnectPeer`, `DisconnectPeer`, `SignMessage`, `VerifySignature`, `GetBalances`, `GetNodeInfo`, `ListForwardedPayments`, `ListChannelForwardingStats`, `GraphGetNode`, `SubscribeEvents`).
- `max_fee_msat` maps to `RouteParametersConfig.max_total_routing_fee_msat`. Leave the other route fields unset, or at ldk-node defaults (check the proto for whether 0 means default; if it does not, fetch the defaults once and fill them in).
- Timeouts: unary calls 30 s, send calls 60 s for the RPC itself. Payment completion is tracked through events and polling, never by blocking the RPC.
- Error mapping: gRPC `PERMISSION_DENIED` → `BackendError::Permission`, `UNAVAILABLE`/transport → `BackendError::Unavailable` (retryable), `INVALID_ARGUMENT` → `BackendError::InvalidRequest(msg)`, failed payments → `BackendError::PaymentFailed(reason)`. ldk-server error codes are in LDKS `error.proto`.
- Known ldk-server gaps (put them in `capabilities()` and treat them as feature flags):
  - `Bolt12ReceiveRequest` has no `issuer` field → `offer_issuer=false`.
  - No hook to decline an invoice request → `single_use_offers=false` (§8.4.18).
  - `PaymentFailed.reason` exists only on the event, not in `GetPaymentDetails`. Persist the reason when the event arrives.

### 5.3 Event stream
- One task owns `SubscribeEvents`. On disconnect it reconnects with exponential backoff (0.5 s → 30 s, with jitter). After **every** (re)connect it triggers a full reconcile (§7.6), because events missed during downtime are not replayed.
- Map `EventEnvelope` to `LnEvent::{PaymentReceived, PaymentSuccessful{preimage, bolt12_invoice}, PaymentFailed{reason}, PaymentClaimable{claim_deadline, claimable_amount_msat}, PaymentForwarded, ChannelStateChanged, SpliceNegotiated, SpliceNegotiationFailed}` and fan out on a `tokio::sync::broadcast` (capacity 1024). A lagged receiver MUST trigger a reconcile instead of being ignored.
- Event handling MUST be idempotent: ledger updates are keyed by `(ln_payment_id, target state)`.

---

## 6. Domain model and ledger (`allod-core`, `allod-db`)

### 6.1 Units
All amounts are `u64` millisatoshis internally (`Msat` newtype). The API exposes `*_msat` fields. NWC uses msat as its spec requires. The UI formats as sats or BIP-177 ₿ depending on the user setting.

### 6.2 Tables (SQL portable across SQLite/Postgres; sqlx migrations in `crates/allod-db/migrations/{sqlite,postgres}`)

```sql
-- key/value settings; encrypted values prefixed "v1:"
config(key TEXT PRIMARY KEY, value TEXT NOT NULL, encrypted BOOL NOT NULL DEFAULT false, updated_at TIMESTAMP NOT NULL)

apps(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL,                       -- 'connection' | 'account'
  app_pubkey TEXT NOT NULL UNIQUE,          -- client pubkey (hex)
  wallet_pubkey TEXT UNIQUE,                -- per-app derived wallet service pubkey; NULL only for legacy shared key
  isolated BOOL NOT NULL DEFAULT false,
  max_amount_msat BIGINT NOT NULL DEFAULT 0,          -- 0 = unlimited
  budget_renewal TEXT NOT NULL DEFAULT 'never',       -- daily|weekly|monthly|yearly|never
  expires_at TIMESTAMP,
  created_by_app_id INTEGER REFERENCES apps(id),      -- set when created via NWC create_connection
  lud16_username TEXT UNIQUE,                         -- for allod-lnurl
  metadata JSON NOT NULL DEFAULT '{}',
  relay_urls JSON,                                    -- per-app override; NULL = global relays
  created_at, updated_at, last_used_at, last_settled_tx_at TIMESTAMP,
  closed_at TIMESTAMP                                 -- accounts only (§10); closed apps answer no NWC requests
)

app_permissions(
  id INTEGER PRIMARY KEY,
  app_id INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  scope TEXT NOT NULL,                      -- see §9.1
  created_at TIMESTAMP NOT NULL,
  UNIQUE(app_id, scope)
)

transactions(
  id INTEGER PRIMARY KEY,
  public_id TEXT NOT NULL UNIQUE,           -- ULID, NWC transaction_id (§6.5)
  app_id INTEGER REFERENCES apps(id) ON DELETE SET NULL,  -- NULL = main wallet
  request_event_id INTEGER REFERENCES request_events(id),
  direction TEXT NOT NULL,                  -- 'incoming' | 'outgoing'
  payment_type TEXT NOT NULL,               -- 'bolt11' | 'bolt12' | 'internal'
  state TEXT NOT NULL,                      -- PENDING|ACCEPTED|CANCELING|SETTLED|FAILED|EXPIRED|CANCELED
  amount_msat BIGINT NOT NULL,
  fee_msat BIGINT NOT NULL DEFAULT 0,
  fee_reserve_msat BIGINT NOT NULL DEFAULT 0,  -- non-zero only while outgoing PENDING
  max_fee_msat BIGINT,                      -- caller cap, if any
  ln_payment_id TEXT,                       -- ldk-server payment_id (hex)
  payment_request TEXT,                     -- bolt11 invoice or bolt12 invoice (lni1...)
  payment_hash TEXT,
  preimage TEXT,
  offer_id TEXT,                            -- bolt12: offer paid (outgoing) or offer received through (incoming)
  offer TEXT,                               -- outgoing bolt12 offer string
  payer_note TEXT,
  quantity BIGINT,
  payer_proof TEXT,
  description TEXT NOT NULL DEFAULT '',
  description_hash TEXT,
  metadata JSON,                            -- NWC-06 metadata (≤ 4096 bytes encoded)
  self_payment BOOL NOT NULL DEFAULT false,
  hold BOOL NOT NULL DEFAULT false,
  settle_deadline INTEGER,                  -- block height (hold invoices, ACCEPTED only)
  claimable_amount_msat BIGINT,             -- hold invoices
  counterpart_tx_id INTEGER REFERENCES transactions(id),  -- internal/self payments: link in<->out
  failure_reason TEXT,
  labels JSON NOT NULL DEFAULT '[]',
  created_at, updated_at TIMESTAMP NOT NULL, expires_at, settled_at TIMESTAMP
)
CREATE INDEX tx_app_created ON transactions(app_id, created_at DESC);
CREATE INDEX tx_hash ON transactions(payment_hash);
CREATE INDEX tx_ln_id ON transactions(ln_payment_id);
CREATE INDEX tx_offer ON transactions(offer_id);
CREATE INDEX tx_state ON transactions(state) ;
-- at most one live outgoing attempt per invoice
CREATE UNIQUE INDEX tx_out_invoice_live ON transactions(payment_request) WHERE direction='outgoing' AND payment_type='bolt11' AND state IN ('PENDING','SETTLED');

offers(
  id INTEGER PRIMARY KEY,
  offer_id TEXT NOT NULL UNIQUE,            -- 32-byte hex
  offer TEXT NOT NULL,
  app_id INTEGER REFERENCES apps(id) ON DELETE SET NULL,  -- owner; NULL = main wallet
  amount_msat BIGINT,                       -- NULL = variable
  description TEXT,
  issuer TEXT,
  single_use BOOL NOT NULL DEFAULT false,
  metadata JSON,
  state TEXT NOT NULL DEFAULT 'active',     -- active | used | expired | disabled
  expires_at TIMESTAMP,
  created_at TIMESTAMP NOT NULL
)

request_events(id INTEGER PRIMARY KEY, nostr_id TEXT NOT NULL UNIQUE, app_id INTEGER, method TEXT, state TEXT NOT NULL, created_at, updated_at)
  -- state: received|executing|executed|error. Request content is NOT stored (privacy); only method.
response_events(id INTEGER PRIMARY KEY, nostr_id TEXT NOT NULL, request_id INTEGER NOT NULL, state TEXT NOT NULL, replied_at, created_at)

notifications_outbox(
  id INTEGER PRIMARY KEY, app_id INTEGER NOT NULL, kind TEXT NOT NULL, transaction_id INTEGER NOT NULL,
  attempts INT NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'pending', next_attempt_at TIMESTAMP, created_at TIMESTAMP,
  UNIQUE(app_id, kind, transaction_id))

api_tokens(id INTEGER PRIMARY KEY, name TEXT NOT NULL, permission TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
           expires_at TIMESTAMP, last_used_at TIMESTAMP, created_at TIMESTAMP)   -- permission: full|readonly

audit_log(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, app_id INTEGER, data JSON, created_at TIMESTAMP)
  -- kinds: nwc_permission_denied, nwc_budget_exceeded, nwc_insufficient_balance, app_created, app_deleted,
  --        app_updated, payment_failed, channel_opened, channel_closed, login_failed, settings_changed

forwards(id INTEGER PRIMARY KEY, ln_forward_id TEXT UNIQUE, inbound_channel TEXT, outbound_channel TEXT,
         outbound_amount_msat BIGINT, fee_earned_msat BIGINT, created_at TIMESTAMP)

-- Double-entry ledger (§6.3). Append-only: rows are never updated or deleted.
ledger_accounts(
  id INTEGER PRIMARY KEY,
  code TEXT NOT NULL UNIQUE,      -- 'wallet:main', 'wallet:app:<app_id>', 'pending:out', 'external:lightning',
                                  -- 'fees:routing', 'equity:node' (see §6.3)
  kind TEXT NOT NULL,             -- wallet | pending | external | fees | equity
  app_id INTEGER UNIQUE REFERENCES apps(id),   -- set for wallet:app:* only
  created_at TIMESTAMP NOT NULL
)
journal_entries(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,             -- payment_reserve | payment_settle | payment_release | receive_settle |
                                  -- transfer | reconcile | reversal
  transaction_id INTEGER REFERENCES transactions(id),
  reverses_entry_id INTEGER REFERENCES journal_entries(id),
  memo TEXT NOT NULL DEFAULT '',
  created_at TIMESTAMP NOT NULL,
  UNIQUE(transaction_id, kind)    -- idempotency: one entry of each kind per transaction
)
postings(
  id INTEGER PRIMARY KEY,
  entry_id INTEGER NOT NULL REFERENCES journal_entries(id),
  account_id INTEGER NOT NULL REFERENCES ledger_accounts(id),
  amount_msat BIGINT NOT NULL,    -- signed; the postings of one entry sum to exactly 0
  created_at TIMESTAMP NOT NULL
)
CREATE INDEX postings_account ON postings(account_id, id);
-- Cached balances, updated in the same DB transaction as each entry; rebuildable from postings.
ledger_balances(account_id INTEGER PRIMARY KEY REFERENCES ledger_accounts(id), balance_msat BIGINT NOT NULL, last_posting_id INTEGER NOT NULL)
```

### 6.3 Double-entry ledger and balances
Money inside Allod is tracked as a double-entry ledger, not by summing transaction rows. `transactions` (§6.2) remains the protocol record of each payment (invoice, hash, preimage, state); the **ledger** says whose money moved where. This differs from Alby Hub, which derives balances from transaction rows.

**Ledger accounts**
| Code | Kind | Meaning |
|---|---|---|
| `wallet:main` | wallet | The Main account: node funds not allocated to an app account. Used by every non-isolated connection. |
| `wallet:app:<id>` | wallet | One per isolated app (user-facing **account**, §10). Created with the app. |
| `pending:out` | pending | Amount plus fee reserve held for outgoing payments in flight. |
| `external:lightning` | external | The outside world: payers and payees on Lightning. |
| `fees:routing` | fees | Routing fees paid on outgoing payments. |
| `equity:node` | equity | Balancing account for changes Allod did not originate: channel opens and closes, forwarding income, on-chain movements, and reconciliation differences. |

**Journal entries** (each entry's postings sum to 0; amounts in msat, `R` = fee reserve, `F` = actual fee):
| Event | Postings |
|---|---|
| Outgoing payment starts (`payment_reserve`) | payer wallet −(A+R), `pending:out` +(A+R) |
| Outgoing payment settles (`payment_settle`) | `pending:out` −(A+R), `external:lightning` +A, `fees:routing` +F, payer wallet +(R−F) |
| Outgoing payment fails (`payment_release`) | `pending:out` −(A+R), payer wallet +(A+R) |
| Incoming payment settles (`receive_settle`) | `external:lightning` −A, receiver wallet +A |
| Internal transfer or self payment (`transfer`) | payer wallet −A, receiver wallet +A (no fee, nothing touches `pending:out`) |
| Internal hold invoice | reserve on the payer like an outgoing payment, then `transfer`-style settle from `pending:out` to the receiver, or `payment_release` on cancel |
| Reconciliation (`reconcile`, §7.6) | `wallet:main` ±D, `equity:node` ∓D, where D = node Lightning balance − Σ(wallet accounts + `pending:out`) |

**Rules**
- Entries are written in the **same DB transaction** as the `transactions` state change they belong to, and only on confirmed state changes (§7). Their `UNIQUE(transaction_id, kind)` makes event replays idempotent.
- Nothing is ever updated or deleted. A mistake is corrected with a `reversal` entry that mirrors the original postings.
- **Account balance** = Σ postings of that account (served from `ledger_balances`, which is updated in the same transaction and can be rebuilt from `postings` with `allod ledger rebuild`).
- **Isolated app balance** = balance of `wallet:app:<id>`. Funds reserved for in-flight payments are already removed, so "spendable" equals the balance. Incoming ACCEPTED (held) payments are not posted until they settle.
- **Main spendable** = balance of `wallet:main` after the latest reconciliation, floored at 0. The UI shows "Node balance", "In accounts", "In flight" and "Main".
- **Invariants**, checked by property tests (§16.1) and by `allod ledger check` at startup and every reconcile:
  1. Every entry sums to 0, so Σ over all accounts = 0.
  2. No `wallet:app:*` balance is ever negative. A negative value is a bug: block payments from that account and raise an `audit_log` alert.
  3. `pending:out` = Σ(A+R) over outgoing transactions in PENDING state.
  4. After reconciliation, Σ(wallet accounts) + `pending:out` = node Lightning balance.
- Statements: `GET /api/accounts/:id/statement` lists postings with a running balance, which the UI shows as a bank-style statement.

### 6.4 Budgets (non-isolated and isolated apps)
- Usage = what the app and all connections it created (recursively) took from their wallet accounts in the period: Σ over those outgoing transactions in {PENDING, SETTLED} with `created_at >= start_of_period(budget_renewal)` of (A + R) while pending and (A + F) once settled. It is computed from the ledger postings tied to those transactions. Every payment checks its own budget and every ancestor's budget under the ledger lock. An expired ancestor blocks delegated spending.
- Period starts use **UTC**: daily 00:00; weekly Monday 00:00; monthly the 1st; yearly Jan 1; never = epoch. The `renews_at` reported to `get_budget` is the next period start.
- If `max_amount_msat > 0` and `usage + amount + fee_reserve > max_amount_msat`, return `QUOTA_EXCEEDED`. Self payments (§6.7) use fee_reserve = 0.
- Delegated connections cannot be hard-deleted: deleting one would set its transaction `app_id` to NULL and erase its spending from ancestor budget queries. Expire the connection to revoke access while retaining its lineage and transaction history.

### 6.5 Transaction identifiers
- `public_id` is a ULID generated at insert. It is the NWC `transaction_id` everywhere (`pay`, `receive`, `lookup_payment`, `list_transactions`, notifications) and it never changes.
- `payment_hash` is still returned and accepted for NIP-47 core compatibility (`lookup_invoice`).
- A self payment creates **two** rows (outgoing for the payer app, incoming for the receiver app), linked by `counterpart_tx_id`. Each connection sees only its own row.

### 6.6 Deviations from Alby Hub (fixes that are REQUIRED)
1. **Hold-invoice ownership**: `settle_hold_invoice` / `cancel_hold_invoice` MUST find the transaction by `payment_hash` **and** `app_id = caller.app_id` (HUB-NOTES §2). Any other owner → `NOT_FOUND`.
2. **settle_deadline**: report `claim_deadline − settle_safety_blocks` (config, default 3) from ldk-server's `PaymentClaimable.claim_deadline`, not raw HTLC expiry (HUB-NOTES §1).
3. **Claimable amount check**: when `PaymentClaimable` arrives, check `claimable_amount_msat >= invoice amount`. If it is lower, persist CANCELING with reason `underpaid`, call `bolt11_fail_for_id` at once, and mark CANCELED only after backend failure is confirmed. Retry unknown outcomes during reconciliation. Send no `hold_invoice_accepted` notification.
4. **BOLT12 receive attribution**: every offer Allod creates is recorded in `offers` with its owner `app_id`. An incoming `Bolt12Offer` payment is credited to `offers.app_id` (looked up by `offer_id`). Payments to unknown offers go to the main wallet. (In Hub's `receive` the offer is not tied to the app.)
5. **Settle failure handling**: if `bolt11_claim_for_id` fails, the row stays ACCEPTED, the NWC error is `INTERNAL` with the backend message, and the reconciler re-checks. The row is never marked SETTLED unless ldk-server reports success or the payment status is `SUCCEEDED`.
6. **Notifications for isolated apps** go only to the owning app (Hub already does this; keep it).
7. **Every request carries its app**. No handler may run a lookup that ignores `app_id` for isolated apps. Write a test per method (§16.3).

### 6.7 Self payments and internal transfers
A payment whose target was created by this Allod instance never touches the network:
- **BOLT11**: an `incoming` row exists with the same `payment_request` and state PENDING, not hold.
  - In one DB transaction: validate the payer (§7.1) with fee_reserve 0, insert the outgoing row as SETTLED (fee 0, preimage from the incoming row), and mark the incoming row SETTLED (`settled_at=now`). Link them via `counterpart_tx_id`. Emit `payment_sent` (payer) and `payment_received` (receiver).
  - Preimage source: normal invoices are created with `bolt11_receive` (ldk-server auto-claims them). Right after creating one, call `payment(id)` and store the preimage that ldk-node reports for its own invoices on the row. If the backend returns no preimage, a self-payment to that invoice is rejected with `INTERNAL: cannot self-pay`. Do **not** create normal invoices through `bolt11_receive_for_hash`: that would make every receive depend on Allod being online to claim it.
  - Self payment to a **hold** invoice: the outgoing row becomes PENDING and the incoming row ACCEPTED (`settle_deadline = NULL`, no block limit for internal holds). Settle → both rows SETTLED. Cancel → incoming CANCELED, outgoing FAILED (`failure_reason="canceled by recipient"`). Internal holds never expire on their own: the reconciler cancels them when the invoice `expires_at` + 24 h passes.
  - If the invoice is already SETTLED or EXPIRED, return `BAD_REQUEST`.
- **BOLT12**: the offer's `offer_id` is in `offers` with state `active`. Create an `internal` payment: generate a preimage, set payment_hash = sha256(preimage), and insert outgoing SETTLED + incoming SETTLED (`payment_type='bolt12'`, `offer_id` set, `payer_note` and `quantity` copied). Check offer amount, quantity and expiry as a real payment would.
- **UI transfer** (`POST /api/transfers {from_app_id?, to_app_id?, amount_msat, description}`) does the same through an internal BOLT11-shaped pair (`payment_type='internal'`), without creating an invoice on the node.

### 6.8 Fee reserve
`fee_reserve_msat = if max_fee given { max_fee } else { max(ceil(amount*0.01), 10_000) }`. Pass this value as the backend's hard maximum routing fee for every external BOLT11 and BOLT12 send. It is held while PENDING and set to 0 on SETTLED (the actual fee goes to `fee_msat`) or FAILED. If a backend reports SUCCEEDED with `fee_msat > fee_reserve_msat`, quarantine the PENDING row and write one `fee_cap_violation` audit alert; do not debit an account below its reservation or book the unsupported fee as normal settlement. Require operator review.

---

## 7. Payment flows (`allod-core::payments`)

### 7.1 Validation (`validate_can_pay`) runs under the **ledger lock**
The ledger lock is a single `tokio::sync::Mutex<()>` held across validation and the PENDING insert. It is not held during the network call. With Postgres, hold a transaction-scoped advisory lock (`pg_advisory_xact_lock(0x414C4C50)`) for every wallet write and use `SELECT ... FOR UPDATE` on budget-chain `apps` rows and the payer's cached ledger balance row during reservation. At daemon startup, hold a separate session advisory lock (`pg_try_advisory_lock(0x414C4C4F)`) on a dedicated connection for the full process lifetime, and refuse to start if it is held.

Checks in order:
1. The app exists, is not expired, and has the `pay_invoice` scope → else `RESTRICTED` / `EXPIRED`.
2. Duplicate: a live outgoing row for the same bolt11 (`PENDING` → `OTHER: payment already in progress`; `SETTLED` → `OTHER: invoice already paid`).
3. Isolated: `amount + reserve <= balance(wallet:app:<id>)` → else `INSUFFICIENT_BALANCE`. Non-isolated: checked against `wallet:main`.
4. Budget (§6.4) → else `QUOTA_EXCEEDED`.
5. Non-isolated: `amount + reserve <= main spendable + (app balance if it has one)` is checked best-effort. Rely on the backend failing if it is wrong, but return `INSUFFICIENT_BALANCE` early when it is clearly short.
6. Insert the outgoing PENDING row and release the lock.

### 7.2 Outgoing BOLT11
1. Decode (`lightning-invoice`). Check network (`bc`/`tb`/`tbs`/`bcrt` against node network) → `UNSUPPORTED_NETWORK` for `pay`, `BAD_REQUEST` for `pay_invoice`. Check expiry → `BAD_REQUEST: invoice expired`. Amount rules: an amount-bearing invoice with a conflicting `amount` param → `BAD_REQUEST`; a zero-amount invoice without `amount` → `BAD_REQUEST`.
2. Self-payment check (§6.7).
3. Validate and insert (§7.1).
4. Each retry is an explicit attempt generation. Before inserting a retry, read the previous backend record and store its `latest_update_timestamp` as the attempt baseline. Insert with submission time unset; set it only after `bolt11_send` returns. A FAILED poll/event may release this attempt only after submission and only when its backend timestamp is strictly newer than the baseline (`>` is intentional because backend timestamps have one-second resolution). An equal timestamp is ambiguous and stays PENDING. SUCCEEDED always settles the newest attempt. If success proves that a row was incorrectly released by older code or a race, repair `FAILED → SETTLED` and debit amount plus actual fee with a compensating journal entry. `bolt11_send` → `payment_id`, stored in `ln_payment_id` at once. ldk-node's bolt11 `payment_id` equals the payment hash. Rely on this for crash recovery, and assert it in an integration test.
5. Wait for the terminal state: subscribe to `LnEvent`s filtered by `payment_id`, and also poll `payment(id)` every 2 s as a fallback. Wait up to `nwc_sync_timeout` (default 55 s).
   - SUCCEEDED → mark SETTLED (`fee_msat`, `preimage`, `settled_at`, fee_reserve=0), set `apps.last_settled_tx_at`, emit `payment_sent`.
   - FAILED → mark FAILED with the reason, release the reserve, respond `PAYMENT_FAILED`.
   - Timeout → respond with an **error** `OTHER: payment still in flight, check later with lookup_invoice/lookup_payment` for `pay_invoice` (NIP-47 has no pending result). For `pay`, respond with a **result** `state: "pending"` (NWC-321 allows it). The row stays PENDING and the reconciler finishes it.
6. A backend error before any HTLC is sent (`INVALID_ARGUMENT`, no route) → FAILED. A transport error with unknown outcome → keep PENDING, and the reconciler resolves it via `payment(id)` (payment_id = hash).

### 7.3 Outgoing BOLT12 (`pay` with `lno`, and the UI)
1. `decode_offer` through the backend (authoritative), plus a local `lightning` decode for fast validation. Check `is_expired` → `BAD_REQUEST`. Check chains against the node network → `UNSUPPORTED_NETWORK`. Amount: an offer amount conflicting with the param or the URI amount → `BAD_REQUEST`. Variable offer without an amount → `BAD_REQUEST`. Non-BTC currency offers → `UNSUPPORTED_PAYMENT_INSTRUCTION`.
2. Self check via `offers` (§6.7).
3. Validate and insert PENDING (`payment_type='bolt12'`, `offer`, `offer_id`, `payer_note`, `quantity`).
4. `bolt12_send` → `payment_id` (random). Store it immediately in the same statement that confirms submission. Crash window: on startup, a PENDING bolt12 row with NULL `ln_payment_id` is matched against `list_payments` (outbound, kind Bolt12Offer, same offer_id, amount, `latest_update_timestamp >= created_at`). Exactly one match → adopt it. None found and the row is older than 10 min → FAILED (`never submitted`). Several matches → leave PENDING and raise an `audit_log` alert for manual review. Never auto-retry.
5. Terminal handling as in §7.2. On success also store `payment_hash`, `preimage`, and `payment_request` (`PaymentSuccessful.bolt12_invoice`), and, if `capabilities.payer_proof`, `bolt12_payer_proof` → `payer_proof` (best-effort: a failure does not fail the payment).

### 7.4 Incoming
- `make_invoice` / `receive` with an amount / UI receive: `bolt11_receive` → insert incoming PENDING (owner app, description, hash, preimage if known, `expires_at`, metadata). With `jit=true` (needs a configured LSPS2 LSP and an amount, else `BAD_REQUEST`): `bolt11_receive_jit` instead — the same row is inserted, and the payment settles through the LSP's just-in-time channel like any other BOLT11 receive; the LSP fee is paid by the payer via the wrapped invoice, so the row's amount stays what was requested. `max_lsp_fee_msat` caps the LSP's total opening fee (ldk-server picks the cheapest offer within the cap; without a cap it picks the cheapest offer).
- `PaymentReceived` (ldk-server auto-claimed): find the row by `ln_payment_id`, else by `payment_hash` and direction incoming.
  - Bolt11 found → SETTLED, `amount_msat` = received amount (it may exceed the invoice amount), `settled_at`.
  - Bolt11 not found (created outside Allod) → insert a new incoming SETTLED with `app_id=NULL`.
  - Bolt12Offer → look up `offers` by `offer_id` → `app_id` = owner (or NULL). Insert incoming SETTLED with `payer_note`, `quantity`, `offer_id`, `payment_hash`, `preimage`, and metadata copied from the offer. If the offer is `single_use` and was `active` → set `used`. If already `used` (a second payment slipped through) → still credit it, since the funds are real, and add an `audit_log` warning.
  - Spontaneous (keysend) payments are **out of scope** (§1.2): no rows, no notifications, no API. If ldk-server receives one anyway, the funds only raise the node balance, and so the main-wallet spendable amount (§6.3).
  - Emit `payment_received` to the owner, or to all non-isolated apps with the notifications scope when `app_id=NULL`.
- Expiry sweep (every 60 s): ordinary incoming PENDING rows past `expires_at` → EXPIRED. A hold row expires only after a backend lookup confirms no payment; a pending held HTLC remains eligible for acceptance.

### 7.5 Hold invoices (NWC-03)
- `make_hold_invoice {amount, description?, description_hash?, expiry?, payment_hash, min_cltv_expiry_delta?}` → `bolt11_receive_for_hash`. Insert incoming PENDING with `hold=true`. If `min_cltv_expiry_delta` is set and the backend cannot honor it, return `NOT_IMPLEMENTED` (ldk-server `Bolt11ReceiveForHashRequest` has no such field: reject non-null values, as NWC-03 allows).
- `PaymentClaimable` → apply the amount check (§6.6.3), then set ACCEPTED, `settle_deadline = claim_deadline − safety`, `claimable_amount_msat`, and notify `hold_invoice_accepted` to the owner only.
- `settle_hold_invoice {preimage}` → hash = sha256(preimage) → row with that hash, owned by caller, `hold=true`, state ACCEPTED (else `NOT_FOUND`, or `BAD_REQUEST: not accepted`). Current block height ≥ `settle_deadline` → still try (the backend decides), but log a warning. `bolt11_claim_for_id(ln_payment_id, claimable_amount_msat, preimage)` → SETTLED, `payment_received` notification. An internal hold (§6.7) settles the pair in the DB.
- `cancel_hold_invoice {payment_hash}` → owner check → PENDING or ACCEPTED → persist CANCELING before backend RPC. External holds become CANCELED only when `bolt11_fail_for_id` succeeds or the backend reports FAILED; an unknown RPC outcome stays CANCELING and is retried during reconciliation. Backend SUCCEEDED instead settles and credits the owning account. NWC reports `TEMPORARILY_UNAVAILABLE` while cancellation is pending. Internal holds cancel and release the paired reserve atomically in the DB.
- The reconciler auto-cancels ACCEPTED holds whose `settle_deadline` has passed (block height), since the HTLC will fail back anyway. Mark them CANCELED with `failure_reason="settle deadline passed"`.

### 7.6 Reconciler
Runs at startup, after every event-stream reconnect, and every 60 s.
1. Outgoing PENDING with `ln_payment_id` → `payment(id)`: SUCCEEDED → apply. FAILED → apply only when it belongs to the current attempt under §7.2; stale or ambiguous failure evidence leaves the reserve held. Not found and older than 10 min → FAILED (`unknown to backend`).
2. Outgoing PENDING bolt12 without id → §7.3 step 4.
3. Incoming PENDING, EXPIRED, or CANCELING hold → `payment(id or hash)`: SUCCEEDED → SETTLED and credit its owner even if local state was CANCELED. FAILED → CANCELED. ldk-server does not re-emit `PaymentClaimable`, so a hold invoice accepted while Allod was disconnected is only visible as an inbound pending payment. For PENDING or EXPIRED hold rows, an inbound pending ldk payment with amount ≥ invoice amount → ACCEPTED with `settle_deadline = NULL` (deadline unknown, sent as `null` to the client), and send `hold_invoice_accepted`. CANCELING rows retry backend failure and remain nonterminal while the outcome is unknown. Expire an unpaid hold only after the backend reports no payment; backend lookup errors never expire it. A CANCELING row with a pending or unknown backend payment blocks node-balance difference booking to Main; one with no backend payment does not.
4. Walk `list_payments` from the newest page down to `config.reconcile_cursor` to find inbound SETTLED payments missing from the DB (missed `PaymentReceived`). Insert them per §7.4. Store the cursor (the latest `latest_update_timestamp` seen).
5. Expire offers (`expires_at < now` → `expired`).
6. Serialize reconciliation runs. Before a node-balance snapshot, read the ledger posting generation and check that no payment is in flight. After the snapshot, take the wallet write lock and a DB write transaction; recheck the generation and in-flight state. Discard a stale snapshot. Otherwise post a `reconcile` journal entry when the node's Lightning balance differs from Σ(wallet accounts) + `pending:out` (§6.3), then run the ledger invariant check.
7. Retry the notifications outbox (§8.6).

### 7.7 On-chain (UI/API only; NWC XX-onchain is milestone M9)
- New address, send (amount or send-all, optional fee rate sat/vB), bump fee, list on-chain payments (`list_payments` kind Onchain).
- On-chain funds always belong to the main wallet. Accounts are Lightning-only in v1.

---

## 8. NWC wallet service (`allod-nwc`)

### 8.1 Transport
- `nostr-sdk` `Client` with the relays from `config.relays` plus every distinct per-app `relay_urls`. Auto-reconnect is on.
- Subscription: one filter per wallet pubkey of a non-expired app, `kinds:[23194]`, `#p:[wallet pubkey]` (one value: Alby's relays reject a filter with several `#p` values), `since: now − 6h` on first connect, then `since: last_seen_created_at − 60s`. Up to 100 filters share one REQ (strfry relays allow 200 filters per REQ by default); above 100 apps, use several REQs. Update the subscriptions when apps are created or deleted (debounce 1 s). A `CLOSED` from a relay for an NWC subscription is logged as a warning.
- **Info events** (kind 13194, replaceable): one per **app wallet key**, signed by it.
  - content: the space-separated methods permitted to that app (§9.2).
  - tags: `["encryption","nip44_v2 nip04"]`; `["notifications", "<types>"]` when the app has the notifications scope; `["extensions","02 03 05 06 08 09 12 321"]`, filtered by the app's permissions and backend capabilities.
  - Republish on app create, permission change, capability change, startup, and every 24 h.
  - **Never** include `lud16` in info events (NWC-08 privacy rule).
- The legacy shared service key also publishes an info event with the union of methods, only if a legacy app exists.

### 8.2 Request pipeline (at most 64 admitted tasks globally and 4 executing per app)
1. At relay ingress, accept only kind 23194 events with at most 32 tags and at most 64 KiB of content plus tag text. Check the author/`p` wallet pair against the current live connection snapshot; unknown pairs are dropped without a database write. Take a global semaphore permit without waiting before spawning a handler task; drop when all 64 permits are in use. Verify the event signature; `created_at` no more than 10 min in the future. Apply a per-connection ingress limit of 120 events per 10 seconds before database work. Refresh the connection snapshot before subscribing for newly created connections.
2. Resolve the app by `event.pubkey` (`app_pubkey`) and check that the `p` tag equals its `wallet_pubkey` again, since it may have been revoked since ingress. Unknown → drop without logging or replying.
3. Dedupe: insert into `request_events(nostr_id)`. A unique violation means the event was already handled → drop it.
4. Encryption: the `encryption` tag (`nip44_v2` | `nip04`; missing → `nip04`). Unsupported → reply with nip44 `UNSUPPORTED_ENCRYPTION`. Decrypt failure → reply with nip44 `BAD_REQUEST`.
5. Parse `{method, params}`. Unknown method → `NOT_IMPLEMENTED`.
6. Staleness: methods that move money, create payment targets, or delegate access (`pay*`, `multi_pay*`, `make_*`, `receive`, `settle/cancel_hold_invoice`, `create_connection`) are dropped without reply if `created_at` is more than 6 h old.
7. Check the `expiration` tag (NIP-40): expired → drop, no reply. Every method requiring `superuser` MUST carry an expiration tag no more than 10 minutes after both event `created_at` and current time; missing or longer expiration → `BAD_REQUEST`. Request-log retention MUST exceed every accepted event's actionable lifetime.
8. Rate limit: token bucket of 30 requests per 10 s per app → `RATE_LIMITED`. Apply it before scope checks so rejected requests cannot write unlimited `nwc_permission_denied` audit rows.
9. App expiry → `EXPIRED`. Scope check (§9) → `RESTRICTED` + audit `nwc_permission_denied`.
10. Dispatch to the handler. Update `apps.last_used_at`.
11. Reply: kind 23195, tags `["p", app_pubkey]`, `["e", request_id]` (plus a `["d", id]` per item for multi methods), same encryption as the request. Publish to the app's relays and record `response_events`. Retry publishing 3× with backoff; if every relay fails, set state `failed`.
- Handler panics are caught (`catch_unwind` / `JoinError`) → `INTERNAL`.

### 8.3 Error codes
`RATE_LIMITED, NOT_IMPLEMENTED, INSUFFICIENT_BALANCE, QUOTA_EXCEEDED, RESTRICTED, UNAUTHORIZED, INTERNAL, UNSUPPORTED_ENCRYPTION, PAYMENT_FAILED, FEE_LIMIT_EXCEEDED, NOT_FOUND, EXPIRED, BAD_REQUEST, OTHER, UNSUPPORTED_PAYMENT_INSTRUCTION, UNSUPPORTED_NETWORK, MULTIPLE_MATCHES, UNSUPPORTED_PAYMENT_TYPE, TEMPORARILY_UNAVAILABLE`.
`allod-core` errors map to NWC codes in one function (`nwc_error_from(CoreError)`). Unit-test every variant.

### 8.4 Methods
Shapes follow NIP47 / NWC files exactly. Below: scope, Allod-specific rules, references.

| # | Method | Scope | Spec | Notes |
|---|---|---|---|---|
| 1 | `get_info` | always granted (node details only with `get_info` scope) | NIP47, NWC-08 | Returns `methods` (permitted), `notifications` (if scope), and with `get_info` scope also `alias, color, pubkey, network` (`mainnet` not `bitcoin`), `block_height, block_hash`. Also `extensions`; `metadata` (app metadata + `id`, `name`); `lud16` if the app has `lud16_username` and LNURL is enabled. |
| 2 | `get_balance` | `get_balance` | NIP47 | Isolated → isolated balance. Otherwise → main spendable (§6.3). msat. |
| 3 | `get_budget` | always granted | NIP47 | `{used_budget, total_budget, renews_at?, renewal_period}` in msat; `{}` when there is no budget. |
| 4 | `pay_invoice` | `pay_invoice` | NIP47 | Params `invoice, amount?, metadata?`. Result `{preimage, fees_paid}`. §7.2. |
| 5 | `multi_pay_invoice` | `pay_invoice` | NIP47 | Pays up to 50 invoices **concurrently** (limit 8 at a time). One response per invoice with tag `["d", id or payment_hash]`. Validation per item under the ledger lock, so the budget cannot be exceeded collectively. |
| 6 | `make_invoice` | `make_invoice` | NIP47 | Params `amount, description?, description_hash?, expiry?, metadata?`. Result is the transaction object. |
| 7 | `lookup_invoice` | `lookup_invoice` | NIP47 | By `payment_hash` or `invoice`. Visible rows only (§9.3). Several visible matches (self-payment pair across non-isolated apps) → prefer the row owned by the caller, otherwise the incoming row. |
| 8 | `list_transactions` | `list_transactions` | NWC-05, NWC-12 | Params `from, until, limit (≤100, default 50), offset, unpaid, type`. Each item uses the NIP-47 transaction object plus the NWC-09 envelope fields (`transaction_id, payment_type, details`). Leave out unpaid invoices unless `unpaid=true`. Order: `created_at DESC`. Visible rows only. |
| 9 | `sign_message` | `sign_message` | NIP47 | Result `{message, signature}`. **Not grantable to isolated apps** (it exposes the node identity key). |
| 10 | `make_hold_invoice` | `make_invoice` | NWC-03 | §7.5. |
| 11 | `settle_hold_invoice` | `make_invoice` | NWC-03 | §7.5. Result `{}`. |
| 12 | `cancel_hold_invoice` | `make_invoice` | NWC-03 | §7.5. Result `{}`. |
| 13 | `create_connection` | `superuser` | Alby ext (HUB `create_connection_controller.go`) | Params `pubkey, name, request_methods, notification_types?, max_amount?, budget_renewal?, expires_at?, isolated?, metadata?`. Rules: cannot grant `superuser`. If the caller has a budget, the new connection's `max_amount` MUST be ≤ the caller's remaining budget and is **required**. All descendants share the caller's budget through §6.4, so sibling allowances are not independent spending grants. A child with a budget cannot renew more often than its caller. An omitted child renewal inherits the caller's renewal. Child expiry cannot exceed caller expiry; an omitted expiry inherits it. The caller cannot create non-isolated connections if it is isolated. `created_by_app_id = caller`. Result `{wallet_pubkey}`. Republish the info event and resubscribe before replying. |
| 14 | `pay` | `pay_invoice` | NWC-321 | §7.2/§7.3. Parse with `bitcoin-payment-instructions`. Prefer `lno` when present and the backend supports BOLT12, else `lightning`. Reject `req-*` unknown params including `req-pop`. A non-empty `payer_note` with a bolt11 selection → `BAD_REQUEST`. `max_fee` honored, so `fees_paid` is always present. Result includes `payer_proof` when available. |
| 15 | `receive` | `make_invoice` | NWC-321 | With an amount → bolt11 **and** a fixed-amount offer in one URI: `bitcoin:?lightning=...&lno=...`. The `transaction_id` refers to the bolt11 row; offer payments create their own rows. Without an amount → a variable-amount offer only (`bitcoin:?lno=...`). Both are tied to the caller (offer → `offers.app_id`). Optional Allod extension params: `jit` (bool) asks for a just-in-time channel invoice (§7.4; needs an amount and a configured LSP) and `max_lsp_fee_msat` caps the LSP's fee (only valid with `jit`, else `BAD_REQUEST`). |
| 16 | `make_offer` | `make_invoice` | NWC-12 | Params `amount?, description?, issuer?, single_use?, expires_at?, metadata?`. `issuer` non-empty and backend lacks issuer → `BAD_REQUEST: issuer not supported`. `single_use=true` and backend cannot decline invoice requests → `BAD_REQUEST: single_use not supported` (do not fake enforcement). `expires_at` → `expiry_secs = expires_at − now` (> 0, else `BAD_REQUEST`). An amount without a description → `BAD_REQUEST`. Reuse: an existing active offer with identical params and the same app MAY be returned. Store in `offers`. |
| 17 | `lookup_payment` | `lookup_invoice` | NWC-09, NWC-12 | Forms: `transaction_id` \| top-level `payment_hash`/`invoice` (bolt11) \| `payment_type` + `lookup`. `bolt12` selectors: `payment_hash` \| `invoice` \| `offer_id` (+ optional `payment_hash`). Visible rows only. Several matches → `MULTIPLE_MATCHES`. Unknown `payment_type` → `UNSUPPORTED_PAYMENT_TYPE`. A preimage is returned only if sha256 matches. `details` per type. `internal` rows report the payment_type of what was paid (`bolt11` or `bolt12`). |
| 18 | `register_push` | `notifications` | NWC-XX (push notifications, proposed: nostr-wallet-connect/nwc#15) | §8.9. Params `endpoint, p256dh, auth, notification_types?, device_name?` (`device_name`: shown to the owner, trimmed to 64 characters, kept when a re-registration omits it). Result `{notification_types}`. Per connection; at most 10 endpoints (`QUOTA_EXCEEDED`); an endpoint already registered by the owner or another connection → `OTHER`. |
| 19 | `unregister_push` | `notifications` | NWC-XX | Params `endpoint`. Result `{}`, also when the endpoint is unknown. |

State mapping NWC ↔ DB: `pending`↔PENDING or internal CANCELING, `accepted`↔ACCEPTED, `settled`↔SETTLED, `failed`↔FAILED, `expired`↔EXPIRED, `canceled`↔CANCELED. NIP-47 core transaction objects use the uppercase/lowercase form required by the spec in use. Check NIP47 (`state` lowercase in current NIP-47) and test with a snapshot.

### 8.5 Notifications (NWC-02, NWC-03)
Types: `payment_received`, `payment_sent`, `hold_invoice_accepted`. Payload `{notification_type, notification: <transaction object + NWC-09 fields>}`.
- Recipients: apps with the `notifications` scope and not expired. Isolated apps receive only their own transactions. Non-isolated apps receive transactions of the main wallet (`app_id NULL`) **and** their own, but not other isolated apps' transactions. (Hub sends main-wallet and all non-isolated app txs to every non-isolated app. Allod narrows this: a non-isolated app sees main-wallet txs + its own. Document it in the UI permission text.)
- Publish kind 23197 (nip44) always. Also publish kind 23196 (nip04) when the app's last request used nip04, or when it has never sent a request (legacy apps).
- Writes go to `notifications_outbox` in the same DB transaction as the state change (transactional outbox). A worker publishes, retrying up to 10 times with exponential backoff, and marks rows `sent` or `dead`.

### 8.6 Connection URIs
`nostr+walletconnect://{wallet_pubkey}?relay={r1}&relay={r2}&secret={client_secret}[&lud16=...]`. When Allod generates the client keypair (UI "new connection"), the secret is shown **once** and never stored (only `app_pubkey` is kept).

### 8.7 NWC-08 client-initiated connections
- HTTP flow: UI route `/connections/new?pubkey=&state=&name=&icon=&return_to=&expires_at=&max_amount=&renewal_period=&request_methods=&optional_request_methods=&notification_types=&isolated=&metadata=` (needs `public_url` for remote use; it works locally too). On approval: create the app with `app_pubkey=pubkey`, then dispatch `nwc:success` (`window` event + `window.opener.postMessage`) with `{relayUrl, walletPubkey, state, lud16?}`, or redirect to `return_to` with query params. On decline: `nwc:declined`.
- Nostr flow: the user pastes or scans `nostr+walletauth://{pubkey}?relay=..&state=..&...` (also `nostr+walletauth+allod://`). The UI shows the confirmation page. On approval, publish a kind 13194 info event signed by the new app wallet key with tags `p`=client pubkey, `state`, `relay`(s), `encryption`, `extensions` incl. `08`, to the relays in the URI **and** the Allod relays.
- Rules (NWC-08): grant every `request_methods` entry or decline; grant a subset of the optional ones (the user can untick them); `max_amount` MUST be enforced with the requested renewal or declined; never grant unrequested methods. `max_amount` in NWC-08 is **msat**.
- Register an OS deep-link handler only in the future desktop build. Web: document the scheme.

### 8.8 NWC-06 metadata
Max 4096 bytes after JSON encoding (`BAD_REQUEST` if larger). Stored on the transaction. Offers carry metadata to payments made to them.

---

### 8.9 Device notifications (Web Push)
Allod notifies the owner's browsers and the installed web app (PWA) of payments with **Web Push**: RFC 8030 delivery, RFC 8291 message encryption (`aes128gcm`) and RFC 8292 VAPID. No Firebase or APNs credentials are involved: Allod signs every push with its own VAPID key, and the browser's push service (Google, Mozilla, Apple) only sees ciphertext and timing. UnifiedPush endpoints speak the same protocol and are a later addition.
- **VAPID key**: a P-256 key derived from the recovery phrase, so a restore keeps every subscription valid. Take the 32-byte secret of `m/1237'/4'/0'` (§4.2) as HKDF-SHA256 input key material with salt `allod/vapid` and info `v1` followed by one counter byte (0, 1, ...); the first output that is a valid P-256 scalar is the key. The public key (65-byte uncompressed point, base64url without padding) is the `applicationServerKey` browsers subscribe with.
- **Subscriptions** (`push_subscriptions`): `owner_pubkey` (→ `owners`, deleted with the owner), `endpoint` (unique), `p256dh`, `auth`, `label`, `events` (comma-separated subset of the §8.5 types, default all), `hide_amounts`, `failures`, `last_error`, `last_success_at`, `created_at`, `updated_at`. An endpoint and its keys only let someone send notifications to that browser; they cannot reach funds or keys, so they are stored in the database (§4.3 covers keys and credentials). At most 20 subscriptions per owner.
- **Recipients**: every owner subscription whose `events` contains the type, for transactions of Main and of every account (the owner sees everything).
- **Outbox**: `push_outbox(subscription_id, kind, transaction_id, attempts, state, next_attempt_at)`, unique per (subscription, kind, transaction), written in the same DB transaction as the state change, next to the NWC rows (§8.5). A worker delivers it with the same retry policy (10 attempts, exponential backoff, then `dead`). Responses: `2xx` sent; `404`/`410` delete the subscription and its queued rows; `400`/`401`/`403`/`413` mark the row `dead` and record `last_error`; `429`, `5xx` and network errors retry.
- **Payload** (JSON, encrypted, at most 3 KB): `{v: 1, type, transaction_id, account, amount_msat, description, created_at}`. `account` is the account name or `null` for Main; `description` is cut to 120 characters. With `hide_amounts`, `amount_msat` and `description` are `null`. A test push has `type: "test"`.
- **Request**: `POST` to the endpoint with `Content-Encoding: aes128gcm`, `TTL: 86400`, `Urgency: high` (`payment_received`, `hold_invoice_accepted`) or `normal`, and `Authorization: vapid t=<JWT>, k=<public key>`; the JWT is ES256 with `aud` = the endpoint's origin, `exp` = now + 12 h, and `sub` = `ALLOD_PUBLIC_URL` when it is `https://`, else `https://github.com/frnandu/allod`.
- **Endpoint rules** (no server-side request forgery): `https://` only, no credentials in the URL. A literal IP must be globally reachable unicast: not loopback, private, link-local, CGNAT, benchmarking, reserved, multicast, documentation, unspecified, or an IPv6 form that embeds such an IPv4 address (mapped, compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, Teredo). A host name must contain a dot and must not end in `.localhost`, `.local`, `.internal`, `.lan` or `.home.arpa` (single-label names such as Docker services are refused). The HTTP client resolves names through a resolver that drops every non-public address and fails when none is left, so the check holds at connection time (names that point at private addresses, DNS rebinding); it ignores proxy settings, follows no redirects, and times out after 10 s. Response bodies are never read: `last_error` and test results hold only the status code or "timed out"/"unreachable".
- **Abuse limits** (an endpoint must not turn Allod against another service): pushes to one host share a token bucket (burst 30, 5/s), each subscription has its own (burst 30, 0.5/s); a push over the limit stays queued with a later `next_attempt_at` and keeps its attempts. At most 500 pending pushes per subscription; later notifications are not queued for it. A subscription is deleted after 50 consecutive failed deliveries. The worker sends one push at a time.
- **Wallet apps (NWC-XX, proposed upstream as nostr-wallet-connect/nwc#15)**: a connection with the `notifications` scope registers its own Web Push or UnifiedPush endpoint with `register_push` (§8.4 #18), and `get_info` then includes `push_vapid_key` (the VAPID public key above). Such a subscription has `app_id` instead of `owner_pubkey`, is deleted with its connection, and receives exactly the notifications the connection receives as relay events (§8.5), no others. The NWC worker queues its push rows when it publishes the notification event, with the payload `{event_id, notification_type, notification}` stored on the row: the NWC-02 payload plus the id of the kind 23197 event (23196 when only NIP-04 was published). When the encrypted message would exceed 4096 bytes, `notification` is cut to `type, state, payment_hash, amount` and `truncated: true` is added; the client then calls `lookup_payment`. Notification events are ephemeral (kinds 20000–29999), so relays do not keep them: `event_id` only lets the client drop the copy that also arrives by relay. The owner sees these devices in Settings → Notifications (with the connection's name) and on the connection's page (Push devices: device name or push host, last delivery, last error; send a test, remove one, or remove all). The extension number is advertised in `extensions` once upstream assigns it.
- **Web app**: the service worker `/sw.js` shows each push as a notification (title from `type` and amount, body from description and account, `tag` = transaction id) and opens Activity when clicked. `/manifest.webmanifest` makes the UI installable; on iOS, Web Push only works after "Add to Home Screen" (iOS 16.4+), and the UI says so.

**Owner alerts** (migration 0017): things that need the owner's attention and are not payments. An alert (`alerts`: `key` unique, `kind`, `title`, `body`, `url`, `created_at`, `dismissed_at`) is raised once per key and queued (`alert_outbox`) for every owner subscription, whatever payment events it chose; the push worker delivers it like a payment push (same encryption, retries, limits, `Urgency: high`) with the payload `{v, type: "alert", kind, alert_id, title, body, url, created_at}`, which the service worker shows as is and opens `url` on tap. The app shows the shown alerts on every page (`GET /api/alerts`) until dismissed (`POST /api/alerts/{id}/dismiss`). Gateways (§12.7) raise: 7 and 2 days before an LNVPS gateway's paid time ends when it does not renew itself, when it has ended, and when automatic renewal fails (with the reason). Keys carry the paid-until time (`gateway:<id>:<expires_at>:7d|2d|expired|renew_failed`), so a renewal clears the alerts about the old date and starts afresh.

## 9. Permissions

### 9.1 Scopes (stored) → methods (granted)
| Scope | Methods |
|---|---|
| `pay_invoice` | `pay_invoice, multi_pay_invoice, pay` |
| `get_balance` | `get_balance` |
| `get_info` | `get_info` node fields |
| `make_invoice` | `make_invoice, make_hold_invoice, settle_hold_invoice, cancel_hold_invoice, receive, make_offer` |
| `lookup_invoice` | `lookup_invoice, lookup_payment` |
| `list_transactions` | `list_transactions` |
| `sign_message` | `sign_message` (non-isolated only) |
| `notifications` | all notification types the backend supports; `register_push, unregister_push` |
| `superuser` | `create_connection` |
Always granted: `get_info` (limited), `get_budget`.

### 9.2 Effective methods
`effective = (scopes→methods ∪ always) ∩ backend-supported`. Backend-supported is derived from `capabilities()`: e.g. without BOLT12, drop `make_offer` and `lno` handling in `pay`/`receive`.

### 9.3 Visibility
- Isolated app: rows with `app_id = self`.
- Non-isolated app: rows with `app_id IS NULL OR app_id = self OR app_id IN (non-isolated apps)`. This matches Hub, where non-isolated apps share the main wallet history. Isolated apps' rows are never visible to other apps.
- The UI/API with a full or readonly token sees everything.

### 9.4 Budget renewal and expiry edits
Editing an app's budget never deletes history. Usage is always computed from transactions (§6.4). Setting `expires_at` in the past disables the app immediately (the next request returns `EXPIRED`).

---

## 10. Accounts

Allod calls Alby Hub's "sub-wallets" **accounts**: separate balances on one node, each usable from its own NWC connection. Every account is a ledger wallet account (§6.3); the node's unallocated funds are the built-in **Main** account.
- An account is an app with `kind='account'`, `isolated=true`, scopes `pay_invoice get_balance get_info make_invoice lookup_invoice list_transactions notifications`, no budget by default, and metadata `{"allod_kind":"account"}`.
- Created from the UI (`POST /api/accounts {name, lud16_username?}`). Allod generates the client keypair and returns the connection URI once. The creation form may then link one existing or new NIP-05 name and/or add a Lightning address on a chosen own domain. New identity usernames are suggested from the lowercase account name, with the next available numbered suffix on that domain when taken; the owner can edit them. If either identity step fails after account creation, the result still shows the connection URI and the identity error, so the owner can finish it in Settings. `lud16_username` remains accepted by the API for older clients.
- Operations in the UI: list (name, balance, last used, linked NIP-05 name and Lightning addresses), detail (statement from the ledger, connection, top-up from Main = internal transfer, withdraw to Main = internal transfer, rename, regenerate the connection (new app_pubkey; old one revoked; balance kept, since the ledger account is tied to `app_id`), close (only at zero balance, or with an explicit "sweep to Main" confirmation)). Account list and detail use a Nostr ostrich icon for linked NIP-05 names and a lightning bolt for addresses; identical NIP-05 and Lightning addresses on one account list row appear once with both icons. Main's list row shows every Lightning address, without a count suffix. The account detail hero shows linked NIP-05 and Lightning addresses as clickable pills; a missing kind shows an Add pill, and no Add pills show when both are present. Adding suggests a username from the account name and the next free numbered variant on the chosen domain. Clicking a linked pill offers edit or removal. Removal asks to delete the identity or only unlink it: NIP-05 unlinks via `account_id = null`; an own-domain or ZEUS Lightning address moves to Main. Alby and registry addresses cannot move to Main with their current connections/offers, so only removal is offered. Own-domain address edits can change username, domain and enabled LNURL/BIP-353 kinds; changing its name or domain rotates its offer and DNS record. Account avatars use the linked NIP-05 pubkey's kind 0 profile picture wherever an account is shown, falling back to its sigil. Main uses the signed-in owner's picture. Rename and regenerate use `PATCH /api/apps/:id` and `POST /api/apps/:id/regenerate`; account scopes cannot be changed, and `DELETE /api/apps/:id` refuses accounts.
- **Closing** (`DELETE /api/accounts/:id`, with `?sweep=true` to move a balance to Main first) sets `apps.closed_at`; the row is never deleted. The ledger account `wallet:app:<id>`, the statement and the transactions stay attached to the account, so its history never turns into Main history (which non-isolated connections can see, §9.3). In one DB transaction under the ledger lock: refuse while the account has an outgoing PENDING payment or an ACCEPTED/CANCELING hold, refuse a non-zero balance without `sweep`, post the sweep as an internal transfer, set `closed_at`, and disable the account's active offers. A sweep needs a fresh NIP-98 signature (§11.1). A closed account answers no NWC requests and gets no notifications, cannot be topped up, renamed or regenerated. A payment can still reach it through an invoice or offer it shared earlier (O-3); `GET /api/accounts` therefore keeps listing closed accounts while they hold funds, and they can be withdrawn.
- `GET /api/accounts/summary` → `{count, total_balance_msat}`.
- Coverage warning: the UI warns when the total of all accounts > node Lightning spendable ("accounts are under-collateralised: open channels or add liquidity"). This is a fractional-reserve warning, so show it prominently.

---

## 11. REST API (`allod-api`)

### 11.1 Auth
**Owner identity.** The owner is one or more Nostr public keys stored in `owners(pubkey TEXT PRIMARY KEY, label TEXT, created_at)` (not encrypted: pubkeys are not secret). The first owner is registered at setup (§3.2). More can be added in Settings → Security (with a signed request from an existing owner) or removed (never the last one).

**Login with a Nostr signer (NIP-98).**
1. `GET /api/auth/challenge` → `{challenge}` (32 random bytes hex, valid 120 s, single use, kept in memory).
2. The UI asks the signer to sign a kind 27235 event: tags `["u", "<origin>/api/auth/login"]`, `["method", "POST"]`, `["payload", sha256(request body)]`; body `{"challenge": "..."}`. Signers supported by the UI: NIP-07 browser extensions (Alby, nos2x, Keys.band, ...), NIP-46 remote signers via `bunker://` or `nostrconnect://` (Amber, nsec.app, ...; the UI reads NIP-44 and NIP-04 encrypted replies and answers in the scheme the signer uses), and a key kept in the browser: the UI can create a fresh key or take an nsec (or an ncryptsec), stores it only in that browser's `localStorage` (never sent to Allod) as a NIP-49 `ncryptsec` under a password of at least 8 characters, and says so before storing it. The decrypted key is held in memory only: a reload, a reboot or signing out asks for the password again. A newly created key is shown as an nsec and must be confirmed as backed up before it is used.
3. `POST /api/auth/login` with `Authorization: Nostr <base64(event)>`. The server checks: valid signature, kind 27235, `created_at` within ±60 s, `u` matches the configured origin (`ALLOD_PUBLIC_URL`, or the request's `Host` for loopback), method and payload hash match, the challenge exists and is unused, and the pubkey is in `owners`. Then it returns `{token, expires_at}`. Malformed or unsigned authorization is rejected without a persistent audit row; failures after signature verification are audited.
- Session token: JWT HS256 signed with the in-memory key (§4.3), claims `{sub: <owner pubkey>, perm: "full", iat, exp: +12h, jti}`. The UI keeps it in `sessionStorage`. `POST /api/auth/logout` adds the `jti` to an in-memory deny list.
- Re-authentication: sensitive actions (reveal the recovery phrase, add or remove an owner, delete an account that still holds funds) need a fresh NIP-98 signature over that exact request, passed in the `Authorization` header in addition to the session (`X-Allod-Session`).
- **API tokens** for scripts and the CLI: `POST /api/tokens {name, permission: "full"|"readonly", expires_in_days?}` returns a random 32-byte token (`allod_` + 64 hex characters) once; only its SHA-256 is stored in `api_tokens`. `GET /api/tokens`, `DELETE /api/tokens/:id`. Sent as `Authorization: Bearer`.
- `readonly` permission: GET routes only. `full`: everything.
- **Lost owner key:** on the host, `allod owner add <npub>` writes a new owner directly into the database. Shell access to the host is the recovery path.
- CSRF: the API only accepts credentials in the `Authorization` header (no cookies), so CSRF does not apply. CORS: same-origin only unless `ALLOD_CORS_ORIGINS` is set.
- Every authenticated mutating route is logged to `audit_log` at the matching kind. Malformed or unsigned login attempts are excluded.

### 11.2 Conventions
JSON only, `snake_case`. Errors: `{error: {code, message}}` with HTTP status 400/401/403/404/409/429/500/503. Lists are paginated with `?limit=&offset=` and return `{items, total}`. Times are unix seconds.

### 11.3 Routes
Public (no auth):
- `GET /api/info` → `{setup_completed, running, version, network?, backend_type:"ldk-server", node_pubkey?}`
- `POST /api/setup` (only while no owner exists), `GET /api/auth/challenge`, `POST /api/auth/login`

Owner session (also before services are running):
- `POST /api/auth/logout`, `GET /api/auth/session`, `GET /api/setup/progress` (seed handoff and sync progress)
- `GET /api/backup` → `{verified}`; `POST /api/backup/verify` `{words: {"<0-based position>": "<word>"}}` (at least 3; compared case-insensitively with the stored phrase; 204, or 400 `WRONG_WORDS`; audit `backup_verified`). Setup stores `backup.verified=false` when Allod generated the phrase and `true` when the owner entered one; a missing key counts as verified (nodes set up before this flag).
- `GET /.well-known/lnurlp/:username`, `GET /lnurlp/:username/callback`, `GET /.well-known/nostr.json` (feature `lnurl`)

Read-only (readonly or full):
- `GET /api/node` (node info + capabilities), `GET /api/node/connection-info` (pubkey, listening addrs), `GET /api/node/status` (synced, block height)
- `GET /api/balances` → `{lightning: {total_msat, spendable_msat, receivable_msat}, onchain: {total_sat, spendable_sat, reserved_sat, pending_from_closures_sat}, accounts_total_msat, main_spendable_msat}`
- `GET /api/channels`, `GET /api/peers`, `GET /api/forwards`, `GET /api/forwards/stats`
- `GET /api/transactions?app_id=&type=&state=&payment_type=&q=&limit=&offset=`, `GET /api/transactions/:public_id`
- `GET /api/offers?app_id=`, `GET /api/offers/:offer_id`
- `GET /api/apps?kind=&q=&order=last_used|created&limit=&offset=` (each item includes balance if isolated, budget usage, scopes), `GET /api/apps/:id`
- `GET /api/accounts` (`{items, total}`), `GET /api/accounts/summary`, `GET /api/accounts/:id`, `GET /api/accounts/:id/statement?from=&until=&limit=&offset=` (ledger postings with running balance, newest first; `:id` = `main` for the Main account)
- `GET /api/onchain/address` (last unused), `GET /api/onchain/transactions`
- `GET /api/settings`, `GET /api/health`, `GET /api/audit?kind=&limit=&offset=`
- `GET /api/logs?type=app|ldk&max_bytes=` (app log tail; the ldk log only when reachable on the same host via a configured path)
- `GET /api/events` (SSE): pushes `transaction.updated`, `balance.changed`, `channel.updated`, `app.updated`, `nwc.request`, `node.status`. Heartbeat every 15 s. Auth via `?token=` because EventSource cannot set headers. Accept short-lived (60 s) **SSE tickets** from `POST /api/events/ticket` instead of the main JWT.

Full only:
- Payments: `POST /api/payments {payment, amount_msat?, max_fee_msat?, payer_note?, from_app_id?}`. `payment` is a bolt11, a bolt12 offer, a BIP-321 URI, a lightning address, a BIP-353 `₿user@domain`, or an LNURL. Result: transaction. `POST /api/payments/decode {payment}` → preview (type, amount, description, expiry, network ok?, min/max for LNURL, `bip353` name when it came from one).
  - **BIP-353 names** (`user@domain`, optionally `₿`-prefixed; not IP hosts or hosts with a port): Allod looks up TXT `user.user._bitcoin-payment.domain` with plain DNS. With no `bitcoin:` record the input is an LNURL address (a `₿` name fails instead). With a record, Allod builds a DNSSEC proof through `dns_resolver` (§3.1, over TCP, `dnssec-prover`) and checks it locally from the root keys and the current time, so the DNS server is not trusted. A record that does not verify, or more than one `bitcoin:` record, is an error and never falls back to LNURL. The verified URI then goes through the normal BIP-321 path (BOLT12 offer preferred, BOLT11 fallback, same ledger reserve, budgets and fee caps); an on-chain-only record is refused. The payment's metadata keeps `recipient_data {identifier: user@domain, bip353: true}` (counterparty source `bip353`). DNS errors without `₿` fall back to LNURL. Not done: the invoice request does not carry the name (`invreq_bip_353_name`), and NWC `pay` does not take names.
- Receive: `POST /api/invoices {amount_msat?, description, expiry?, app_id?, jit?, max_lsp_fee_msat?}` (`jit` asks for a just-in-time channel invoice, §7.4); `POST /api/offers {amount_msat?, description, expires_at?, app_id?}`; `POST /api/receive {amount_msat?, description, jit?, max_lsp_fee_msat?}` → unified BIP-321.
- Transfers: `POST /api/transfers` (§6.7)
- On-chain: `POST /api/onchain/address` (new), `POST /api/onchain/send {address, amount_sat? , send_all?, fee_rate_sat_vb?}`, `POST /api/onchain/bump-fee {txid, fee_rate_sat_vb}`
- Channels: `POST /api/channels {pubkey, address, amount_sat, push_msat?, public, fee_base_msat?, fee_ppm?}`, `PATCH /api/channels/:user_channel_id {fee_base_msat?, fee_ppm?, max_dust_htlc_exposure?, cltv_expiry_delta?}`, `DELETE /api/channels/:user_channel_id?counterparty=&force=`, `POST /api/channels/:id/splice-in {amount_sat}`, `POST /api/channels/:id/splice-out {address, amount_sat}`
- Peers: `POST /api/peers {pubkey, address, persist}`, `DELETE /api/peers/:pubkey`
- Apps: `POST /api/apps {name, kind?, pubkey?, scopes[], max_amount_msat?, budget_renewal?, expires_at?, isolated?, metadata?, relay_urls?, lud16_username?}` → `{app, pairing_uri?}` (the URI exists only when Allod generated the keypair); `PATCH /api/apps/:id`; `DELETE /api/apps/:id`; `POST /api/apps/:id/regenerate` → new pairing URI.
- NWC-08: `POST /api/apps/authorize {…NWC-08 params…, granted_optional_methods[], approve: bool}` → `{wallet_pubkey, relay_url, lud16?}` (and the backend publishes the 13194 event for the Nostr flow).
- Accounts: `POST /api/accounts {name, lud16_username?}` → `{account, pairing_uri, bip353?}`, `POST /api/accounts/:id/topup {amount_msat}`, `POST /api/accounts/:id/withdraw {amount_msat}`, `DELETE /api/accounts/:id?sweep=` (re-auth when sweeping a balance)
- Transactions: `PATCH /api/transactions/:public_id/labels {labels[]}`
- Sign: `POST /api/sign-message {message}`
- Device notifications (§8.9): `GET /api/push` → `{vapid_public_key, events, subscriptions: [{id, label, endpoint_host, events, hide_amounts, created_at, last_success_at, last_error, failures}]}` (endpoints and keys are never returned); `POST /api/push/subscriptions {endpoint, keys: {p256dh, auth}, label?, events?, hide_amounts?}` (owner session; upsert by endpoint, so a browser re-registers on every visit and learns which subscription is its own); `PATCH /api/push/subscriptions/{id} {label?, events?, hide_amounts?}`; `DELETE /api/push/subscriptions/{id}`; `POST /api/push/subscriptions/{id}/test` → `{delivered, status?, error?}` (sends at once, outside the outbox).
- Settings: `PATCH /api/settings {relays?, currency?, fiat_source?, bitcoin_display_format?, settle_safety_blocks?, lnurl_domain?, theme?}`; `POST /api/backup/mnemonic {password}` → recovery phrase (re-auth);
- Owners: `GET /api/owners`, `POST /api/owners {pubkey, label}` (re-auth), `DELETE /api/owners/:pubkey` (re-auth, never the last one); `GET /api/backup/export {password}` → encrypted JSON export of the DB (apps, permissions, offers, transactions; *no* ldk-server state).
- Tokens: §11.1. `POST /api/stop`.

Write `openapi.yaml` with `utoipa` and generate the TS client for `web/` from it (`openapi-typescript`). The OpenAPI document is the contract between backend and frontend agents.

---

## 12. Receive identity (`allod-lnurl`, feature-gated, milestone M8)

- Needs `public_url` with HTTPS (reverse proxy). Settings: `lnurl_domain`.
- LUD-16: `GET /.well-known/lnurlp/{username}` → payRequest (min 1 sat, max 1 BTC, `commentAllowed: 255`, `allowsNostr: true`, `nostrPubkey` = Allod zap key `m/1237'/2'/0'`). Answered only when the `Host` header is a served domain (§12.1) and the username is an address on it; anything else is a 404 LNURL `ERROR`. Metadata: `[["text/plain","Sats for <address>"],["text/identifier","<address>"]]`.
- Callback `GET /lnurlp/{username}/callback?amount=&comment=&payerdata=&nostr=` creates a bolt11 invoice owned by the address's account (or Main), 1 h expiry, `description_hash` = sha256 of the metadata string, or of the zap request exactly as sent for a zap; the invoice description (shown in Allod) is the zap message or comment. The transaction's metadata keeps `comment`, `payer_data`, `nostr` (NWC-06 shape) and `zap_request` (the raw JSON). Answer `{pr, routes: [], verify}`.
- LUD-21: `GET /lnurlp/{username}/verify/{payment_hash}` → `{status: "OK", settled, preimage, pr}` for that address's own invoices only.
- All three answer with `Access-Control-Allow-Origin: *` (LUD-01). Code: checks in `allod-lnurl::server`, address lookup and invoices in `allod-core::domains::lnurl`, routes in `allod-api::routes::lnurl`.
- NIP-57: with `nostr`, the zap request must have a valid signature, kind 9734, exactly one `p` tag, a `relays` tag, at most one `e` tag, and an `amount` tag equal to the amount when present. On settle, `allod-nwc` signs the kind 9735 receipt with the zap key (`p`/`e`/`a` copied, `P` = zapper, `bolt11`, `description` = the raw zap request, `preimage`, `created_at` = settle time) and sends it to the zap request's relays (at most 10). Its event id goes into the metadata (`zap_receipt`); unsent receipts of the last 3 days are retried every 10 minutes and after a restart.
- BIP-353: one TXT record per address with the address's own offer (§12.1); Allod writes it through a DNS provider or lists it to add by hand. Without a domain, names come from twelve.cash or clank.money (§12.3, §12.4).
- NIP-05: `GET /.well-known/nostr.json?name=<username>` on a served own domain returns `{"names":{"<username>":"<hex pubkey>"}}` with `Access-Control-Allow-Origin: *`. Without `name`, it returns all mappings for that domain. Unknown names return an empty `names` object; unknown or unserved hosts return 404. Names are separate from Lightning addresses and may map to any valid `npub`; the NIP-05 root name `_` is allowed. A name may link to one open account (`nip05_names.account_id`, migration 0019); one name per account, and deleting an account row clears the link. Settings → Nostr names (NIP-05) lists mappings with avatars and linked accounts; top-right Add and Edit dialogs choose a served own domain, username, npub, and optional account. When signed-in owner's key has no mapping, Add suggests their npub and profile username; user edits are not overwritten. `GET/POST /api/nip05`, `PATCH/DELETE /api/nip05/{id}` are owner-session endpoints; writes need full owner permission. PATCH can change domain, username, npub, or account (null unlinks). `nip05_names` is unique per `(domain_id, username)` and cascades on domain removal. NIP-05 is not offered for registry names or unserved domains.
- Lightning address username rules: `^[a-z0-9._-]{1,64}$`, unique, reserved list (`admin, root, allod, api, www, ...`). NIP-05 uses the same rules, except `_` is allowed for the domain identity.

### 12.1 Domains, DNS providers and addresses

Receiving at a Lightning address on the owner's own domain. Domains, DNS providers and gateways (§12.7) are entities of their own, each with a Settings page; addresses use them. Owners without a domain use twelve.cash, clank.money, ZEUS Pay or Alby (§12.3–§12.6). (Free dynamic DNS was dropped in 2026-11: deSEC suspended new `dedyn.io` names, and no other free dynamic DNS zone is DNSSEC-signed.)

**Tables** (migration 0013, SQLite and Postgres; code `allod_core::domains`):
- `dns_providers (id, kind, label, created_at, checked_at, error)`: an account at a DNS host with an API. `kind`: `njalla` (others later). The API token is the secret file `dns_provider_<id>` (never in the database). "Manual" is not a row: a domain without a provider is managed by hand (MyNymBox and any host without an API).
- `domains (id, name UNIQUE, dns_provider_id NULL, gateway_id NULL, direct, created_at, changed_at, managed, synced_at, error, reach_state, reach_checked_at, reach_detail)`. `name` is normalized (`pay.example.com`). How the domain is **served** over HTTPS (LNURL, certificate):
  - `gateway_id` set: its A record points at that gateway's IPv4 (§12.7); the home IP stays hidden.
  - `direct`: the owner points it at this machine's public IP (port 443 forwarded); the home IP becomes public (privacy warning before choosing it).
  - neither: not served; its addresses answer BIP-353 only (no A record, nothing connects to this machine, works behind CGNAT).
  `managed` (JSON) maps `"TYPE name@zone"` → `{id, zone, kind, name, content}` for records Allod created at the provider.
- `ln_addresses (id, domain_id, username, app_id NULL, offer_id NULL, verified, verified_at, created_at, lnurl, UNIQUE (domain_id, username))`: one address `username@domain`, paying into an account (`app_id`) or Main (NULL). `lnurl` (default on, migration 0014): it also answers LNURL when its domain is served; off, it is BIP-353 only even on a served domain. `bip353` (default on, migration 0015): it publishes its TXT record; off, it is LNURL only. At least one stays on. `PATCH /api/addresses/{id} {lnurl?, target?}` switches it, and `target` (`main` or an open account id) moves the address to another account: its offer belongs to the account it pays, so the old one is disabled and a new one, owned by the new account, goes into the TXT record (synced at the DNS provider, or shown to add by hand); LNURL follows at once. Addresses on registries and services (§12.3–§12.6) cannot move. The address dialog offers "Wallets with BIP-353" or "Every Lightning wallet"; choosing the latter on a domain that is not served also asks how to serve it (a gateway, or this machine). Usernames are per domain: the same username may exist on two domains with different accounts. Rules: `^[a-z0-9._-]{1,64}$` and the reserved list (§12).

**Migration from the single public name** (once, at start, when the tables are empty and `config.public_name` has `mode: own`): the name becomes a domain; `exposure: public` becomes `direct`; a connected Njalla token becomes provider 1 (`dns_token` → `dns_provider_1`) with the domain on it and its `managed` records; an `a_record` becomes a manual gateway (§12.7) with that IPv4, serving the domain; Main's username and each open account's `lud16_username` become addresses with their existing offers. The legacy fields are then cleared. `apps.lud16_username` is no longer read; account creation with `lud16_username` adds an address on the first domain (by id).

**`ALLOD_PUBLIC_URL`**: its domain is a domain row marked `env` (served `direct`, cannot be removed or changed in the UI).

**Addresses and BIP-353**: each address has its own offer (variable amount, description `₿username@domain`, metadata `{"bip353": address}`), owned by its account. Its TXT record is `<user>.user._bitcoin-payment.<domain>. IN TXT "bitcoin:?lno=<offer>"`. A closed account's addresses are removed. Removing an address disables its offer.

**DNS providers** (kinds `njalla` and `desec`). Njalla (API as the lego ACME client uses it: `POST https://njal.la/api/1/` with `{"method", "params"}` and `Authorization: Njalla <token>`; `list-records {domain}`, `add-record {domain, type, name, content, ttl}`, `remove-record {domain, id}`):
- Adding a provider checks the token (`list-domains`, or `list-records` of a domain given) and stores it. Removing one is refused while domains use it.
- **deSEC** (kind `desec`, free DNS hosting that signs every zone; works with a domain from any registrar once its name servers are `ns1.desec.io` and `ns2.desec.org`): REST at `https://desec.io/api/v1/` with `Authorization: Token <token>`. The token is checked with `GET domains/`. Records come in sets (name and type → values): Allod adds or removes one value by rewriting its set (`GET domains/{zone}/rrsets/{subname or @}/{type}/`, then `PATCH domains/{zone}/rrsets/` with that one set; an empty set deletes it), so the owner's other values stay. TXT values go as quoted strings of at most 255 bytes. The zone is the account's longest domain that ends the name. Record ids are synthetic (`type|name|content`). Requests answered 429 are retried after `Retry-After` (at most 15 s, three times); TTLs are at least 3600. The domain checks also read `GET domains/{zone}/` and the domain's view carries its DS values (`ds`, SHA-256 first), which the domain dialog shows for the registrar, with a guide to delegating a domain to deSEC in the add-provider dialog.
- **A deSEC account made by Allod**: the owner solves deSEC's captcha (`POST /api/dns-providers/desec/captcha` → `{id, challenge}`, a base64 PNG; `POST /api/dns-providers/desec/signup {captcha_id, solution}`). Allod registers (`POST auth/`, `outreach_preference: false`) with the gateway account key's own address `npub…@uid.ovh` (§12.8, the same mailbox as LNVPS) and a random password (secret `desec_password`); progress is `config` key `domains.desec_signup` `{email, state: activating|done|failed, since, error, provider_id}`, shown as the view's `desec_signup`. The background task, every 15 s while activating, reads the mailbox for `…/v/activate-account/<code>/`, POSTs it, logs in (`auth/login/`), creates a token `Allod` that may create domains (`auth/tokens/`, `perm_create_domain`), logs out and adds the provider. No mail within 12 hours (the link's lifetime): `failed`. `GET /api/dns-providers/desec/credentials` gives the email and password so the owner can also use desec.io. With a deSEC provider, a domain whose zone is not in the account yet is created there (its last two labels); the owner then sets the registrar's name servers and DS record, both shown in the domain's dialog.
- **A Njalla account made by Allod** (`POST /api/dns-providers/njalla/signup`, nothing to solve): Njalla has no account API, so Allod uses its website with a cookie session: `GET /signup/`, then posts the form (`email` = the same `npub…@uid.ovh`, a random password in secret `njalla_password`, `password_confirm`, `xmpp=0`, the CSRF field, with a `Referer`). The mail "Njalla: email confirmation" carries `https://njal.la/a/<signed code>`; opening it confirms the address and logs the session in. Allod then calls the site's own `add-token` (`POST /api/1/` with the session cookie, `X-CSRFToken` and `{"comment": "Allod"}` without limits), checks the key with `list-domains` and adds the provider. When a session is not logged in, Allod signs in at `/signin/` with the site's proof of work (`cnonce` such that `sha256(email + password + cnonce)` starts with the page's `nonce`). Progress: `config` key `domains.njalla_signup`, view `njalla_signup`; `GET /api/dns-providers/njalla/credentials` gives the login for njal.la (where the owner can buy domains, paid in bitcoin).
- **Removing a provider Allod's sign-up made** marks the sign-up `removed` (a `done` sign-up whose provider is gone is read the same way): the account and its password remain. Signing up again first logs in to that account and adds a new token (deSEC: `auth/login/`, no captcha needed; Njalla: `/signin/`); only when that fails (the account was deleted) does it register a new one.
- **Buying a domain at Njalla** (Domains → Buy; needs a Njalla provider): `GET /api/njalla/search?provider_id=&q=` lists names (`find-domains`: `{name, status, price}`, euros a year; status `available`, `taken`, `unavailable`, `own`, `in progress`, `failed`). `POST /api/njalla/buy {provider_id, domain, years?, gateway_id?, direct?}` checks the name is a registered name (two labels) Njalla sells (`get-tlds`: price, `max_years`, `dnssec`) and that `find-domains` says `available` (a failed or pending check is refused: money would wait in the Njalla wallet for nothing). When the Njalla wallet (`get-balance`, euros) is short, Allod asks for a bitcoin top-up (`add-payment {amount, via: "bitcoin"}`; 5 € or a multiple of 15 €, at most 300 €), which returns an on-chain address, the BTC amount and a BIP21 URI. The owner pays it from any wallet (QR) or from the node (`POST /api/onchain/send`, after a confirmation that names amount and address); Allod never pays it by itself. Progress is `config` key `domains.njalla_purchase` (view `njalla_purchase`: `topup` → `registering` → `done`, or `failed`), followed every 15 s: once the balance covers the price, `register-domain {domain, years}` (a task, `check-task`); done when `get-domain` finds it. Then DNSSEC is turned on (`edit-domain {dnssec: true}`, when the TLD supports it) and the domain is added to Allod with that provider and the chosen serving, so its A and TXT records follow. `DELETE /api/njalla/buy` stops waiting for a top-up (money sent stays in the Njalla wallet), and on a finished purchase (done or stopped) forgets it, so the card closes for good; a registration under way cannot be stopped. No top-up within 3 days, or no registration within a day: `failed`.
- Each reconcile syncs every domain with a provider: one TXT per address, and an A record for the domain when it is served through a gateway (the gateway's IPv4). For Njalla the registered zone is the name's last two labels; the part below it is the record's name (`@` for the zone itself). Only records Allod created (`managed`) are changed or removed; an identical record already there is adopted. Allod's A record replaces other A records on that name. Removing a domain, or moving it to another provider, removes Allod's records from the old one.
- Calls that cannot reach the provider are retried twice (after 2 s and 4 s); a domain whose sync failed is synced again after 1 minute instead of 5.
- **Manual domains**: Allod lists the records to add (TXT per address, and the A record when served through a gateway), with copy buttons.

**Checks** (each reconcile, and the quick checks of new names 10 s, 30 s, 2 min and 5 min after a change): TXT records are asked from the zone's own name servers (walking up to the closest parent with NS records) and an address is `verified` when its record carries its offer. A served domain's reachability is checked by fetching `https://<domain>/.well-known/allod-reach` (through `caddy:443` when `ALLOD_TLS_CHECK_ADDR` is set): `waiting_for_certificate` for 10 minutes after a change, then `reachable` or `not_reachable` with the error.

**Live**: an address is live for BIP-353 when `verified`, and for LNURL when `lnurl` is on and its domain is served and `reachable`. In the address list its privacy signals follow how it is reached: through a gateway with LNURL like the relay option (nothing exposed), from this machine like the home-served option (home IP public), BIP-353 only like the own-domain option. A domain has its own checks (`checks` in the view), run each reconcile whatever addresses it has: `dns` (the closest ancestor zone with NS records answers), `dnssec` (its parent publishes a DS record) and, with a DNS provider, `provider` (the token can list the zone). A domain is live when these pass and, when served, it is `reachable`. The domain dialog shows each check with the fix (e.g. turn DNSSEC on at the registrar), on a Settings tab next to a DNS records tab.

**TLS**: Caddy obtains certificates with TLS-ALPN-01 on 443 (no port 80, no DNS plugin). The Caddyfile's catch-all site uses `tls { on_demand }` with `ask http://allod:8029/api/internal/tls-ask`; Allod answers 200 only for a served domain (and `ALLOD_DOMAIN`). The `ask` route needs no auth; Caddy answers `/api/internal/*` with 404 on public sites. On address domains (the on-demand site) only `/.well-known/lnurlp/*` and `/lnurlp/*` reach Allod; Caddy answers `/.well-known/allod-reach` itself (the reachability check) and 404 for everything else, so the web UI and the API are never exposed there: they stay on `ALLOD_DOMAIN` (or the local port). Gateways cannot filter paths: they pass TLS through without decrypting it.

**API** (owner session):
- `GET /api/domains` → `{domains: [Domain], dns_providers: [Provider], gateways: [Gateway]}`. Domain: `{id, name, env, dns_provider_id, gateway_id, direct, served, live, reach, synced_at, error, addresses: [Address], records: [{type, name, value, managed, ok}]}`; Address: `{id, username, address, domain_id, target ("main" | account id), account_name, txt, verified, lnurl}`.
- `POST /api/domains {name, dns_provider_id?, gateway_id?, direct?}`; `PATCH /api/domains/{id} {dns_provider_id?, gateway_id?, direct?}` (`null` clears); `DELETE /api/domains/{id}` (removes its addresses and Allod's records).
- `POST /api/domains/{id}/addresses {username, target}`; `DELETE /api/addresses/{id}`.
- `POST /api/dns-providers {kind, label?, token}`; `PATCH /api/dns-providers/{id} {label?, token?}`; `DELETE /api/dns-providers/{id}`.
- `POST /api/domains/check`: reconcile now.

**Settings → Domains** (UI): the list of domains with a live badge, how each is served (gateway name, "this machine", or "BIP-353 only") and its provider ("Manual" when none); "+ Add domain" (name, provider, how it is served). A domain's page shows its addresses, the records (managed by the provider, or to add by hand with copy buttons), sync and reachability, and change provider / serving / remove. A **DNS providers** section lists provider accounts (kind, label, domains on it, last error) with add / edit token / remove, and explains manual management.

**Settings → Lightning addresses**: adding an address on an own domain asks for the domain (from the list, or "add a domain"), the username and the account. The questions flow (§13) leads to the domain or gateway pages when they are missing.

**Origin for sign-in**: NIP-98 `u` (§11.1) accepts the effective public URL plus the loopback `Host`.

**Privacy**: serving `direct` shows the warning that anyone who sees the address can learn the home IP; gateways and BIP-353 only avoid this. The UI also checks what the node announces in Lightning gossip (a clearnet `ALLOD_PUBLIC_IP` publishes the IP anyway).

### 12.3 twelve.cash addresses (BIP-353 without a domain)

No free dynamic DNS zone is DNSSEC-signed (checked 2026-11: dynv6, DuckDNS, Dynu, YDNS, FreeDNS, nsupdate.info, now-dns), so none can carry BIP-353. twelve.cash (MIT, ATLBitLab) is a DNSSEC-signed BIP-353 registry with an API, so it gives Main and every account a payable address with no domain, DNS or open port.

- **One offer per name**: every name Allod registers or links (twelve.cash, clank.money §12.4) points at an offer of its own, owned by the target, with metadata `{"bip353": <address>}` so it is never reused as the target's general long-lived offer (§12.1 records). When the name is known first, the offer's description is the name, `₿user@domain`, so payers' wallets show which address they paid.
- **Free names**: `POST /api/public-name/twelve-cash {target: "main" | <account id>}`. Allod makes the target's twelve.cash offer (metadata `{"bip353": "twelve.cash"}`; twelve.cash picks the random name only after it has the offer, so the description cannot carry it and reads `Payment to <account> (twelve.cash)`, or `Payment (twelve.cash)` for Main) and calls tRPC `payCode.createRandomPayCode` (`POST https://twelve.cash/api/trpc/payCode.createRandomPayCode`, body `{"json": {"domain": "twelve.cash", "lno": <offer>}}`) without authentication. The answer's `result.data.json.userName` gives `adjective.animal@twelve.cash`, whose TXT record is `bitcoin:?lno=<offer>`.
- **Buying a chosen name** (`POST /api/public-name/twelve-cash/buy {target, username, max_price_sats, pay_from?}`; 5,000 sats at the time of writing): Allod does what the website does. `payCode.createPayCode {userName, domain, lno}` reserves the name as PENDING (`CONFLICT` = taken); an MDK checkout is created through the site's `POST /api/mdk` (`handler: create_checkout`, `type: AMOUNT`, `amount: 5000`, `currency: SAT`, `metadata.payCodeId`; then `confirm_checkout` when it comes back without an invoice), sending the `mdk_csrf` cookie and the matching `x-moneydevkit-csrf-token` header like MDK's browser client. Allod checks the invoice is BOLT11 for exactly the price, on the right network, not expired and within the cap, records the purchase (`purchase: {pay_code_id, checkout_id, payment_id, price_sats, error}`) before paying, pays through the normal wallet path (§6–7), and only after its payment settled and `get_checkout` reports `PAYMENT_RECEIVED` calls `payCode.redeemPayCode`, which publishes the name. twelve.cash's server publishes on `redeemPayCode` without checking payment itself (it relies on the browser), so Allod never calls it earlier. A purchase that cannot finish at once is finished by the background task from the stored payment; it never pays twice. Its offer is the name's own (description `₿name@twelve.cash`).
- **Custom names bought on the website** can still be linked. The owner types the name first; `GET /api/public-name/twelve-cash/offer?target=&address=` returns that name's offer (description `₿name@twelve.cash`, the same offer when asked again) to paste on twelve.cash; `POST /api/public-name/twelve-cash {target, address}` then links the bought name after a DNS lookup shows its TXT record carries `lno=<offer>`.
- Stored in `config.public_name` (`twelve`: target → `{user_name, domain, offer_id, custom, verified, created_at}`); one address per target. Allod marks a new address `verified` once its TXT record carries its offer, asked from the registry's own name servers (NS of `twelve.cash` / `clank.money`, no cache), because a recursive resolver can keep an early "no such name" for the zone's negative TTL (30 min on twelve.cash). New names are checked 10 s, 30 s, 2 min and 5 min after they are added, then with every 5-minute reconcile; `POST /api/public-name/names/check` (the cards' "Check now") checks at once, and the page refreshes every 10 s while a name waits. It reports an address inactive once its offer is no longer active (account closed). `DELETE /api/public-name/twelve-cash/{target}` only forgets it: twelve.cash has no way to edit or delete a name, so a closed account's address stays published and fails to pay; the UI says so.
- Mainnet only (twelve.cash publishes mainnet offers); other networks answer `409`. `ALLOD_TWELVE_CASH_URL` (hidden, testing) points at a stand-in and lifts the restriction.
- Trust: twelve.cash controls the record and could point a name at another offer. The UI names it as a third party.

### 12.4 clank.money names (paid, chosen)

clank.money is a DNSSEC-signed BIP-353 registry where the owner picks the name and pays once over L402 (999 sats at the time of writing). Unlike twelve.cash it returns a management token, so the name can later point at another `bitcoin:` URI.

- **Buy**: `POST /api/public-name/clank {target, username, max_price_sats, pay_from?}` (`pay_from`: `main` or an account id, default the target). Usernames: 3–32 of `a-z 0-9 -`, no leading or trailing hyphen. `GET /api/public-name/clank/available?username=` asks `GET https://clank.money/api/v1/registrations/{username}` (404 = free; other answers = unknown).
  1. Allod makes the name's offer (description `₿username@clank.money`, metadata `{"bip353": "username@clank.money"}`) and sends `POST /api/v1/registrations {username, bip321Uri: "bitcoin:?lno=<offer>"}` without credentials. `409` = taken. `402` carries `macaroon` and `invoice` (also read from `WWW-Authenticate: L402 macaroon=…, invoice=…`) and `amountSats`.
  2. Before paying, Allod decodes the invoice: BOLT11, right network, not expired, amount equal to `amountSats` and at most `max_price_sats`. Otherwise it stops and disables the new offer; nothing is paid.
  3. It stores the entry (`paying`, the invoice) and the macaroon (`{data_dir}/secrets/clank_l402_<target>`) before paying, then pays through the normal wallet path (`pay_from`'s balance and budget, fee reserve, ledger lock; §6–7) and waits up to 60 s.
  4. Settled: Allod repeats the POST with `Authorization: L402 <macaroon>:<preimage>`. `201` or `202` (paid, DNS still propagating) returns `managementToken`, stored in `{data_dir}/secrets/clank_token_<username>`; the macaroon file is removed and the entry becomes `registered`. Failed payment: the entry and macaroon are dropped and the offer disabled.
  5. If the payment is still pending, or the paid registration call fails, the entry stays `paying`/`paid` with the error, and the background task (every reconcile) finishes it from the stored payment id and macaroon. It never pays twice.
- Stored in `config.public_name` (`clank`: target → `{user_name, offer_id, state, invoice, payment_id, price_sats, verified, created_at, error}`); one name per target. `verified` and `active` work as for twelve.cash.
- `DELETE /api/public-name/clank/{target}` forgets a `registered` name and deletes its management token (the name stays at clank.money, pointing at the offer). Not built yet: pointing a name at another offer with `PATCH /api/v1/registrations/{username}` (the client has it), e.g. when an account closes.
- Mainnet only, like twelve.cash; `ALLOD_CLANK_URL` (hidden, testing) points at a stand-in and lifts the restriction.
- Trust: clank.money controls the record. Its documentation is short (written as an agent skill); the price and the 402 body format are taken from it and checked against the invoice before paying.

### 12.5 ZEUS Pay addresses (LNURL without a domain)

ZEUS Pay (zeuspay.com, run by the ZEUS wallet team) gives a free LNURL Lightning address on its domain whose invoices come from this node over NWC, so every Lightning wallet can pay it, with no domain, no open port and the node's IP kept private. Its API is not documented; Allod follows the open-source ZEUS app (`stores/LightningAddressStore.ts`, AGPLv3).

- **One per node**: ZEUS Pay keys the address to the node pubkey, so the node has one (`config.public_name.zeus`: `{handle, domain, target, connection_id, verified, created_at}`). It pays into the account the owner picks (`target`: `None` = Main): the connection's metadata carries `account`, so its invoices belong to that account and the connection sees them through `via_app_id`, as for Alby (§12.6). `PUT /api/public-name/zeus/target {target}` changes it later (the connection's name and metadata are rewritten; nothing changes at ZEUS Pay). ZEUS Pay+ is always paid from Main.
- **Auth**: `POST https://zeuspay.com/api/lnurl/auth {pubkey}` returns `verification`, 64 random bytes as lowercase hex. Allod signs only a challenge of exactly that shape, with the node key (`SignMessage`, LND-compatible zbase32), so the node key never signs server-chosen text. Every signed call carries `{pubkey, message, signature}`.
- **Add** (`POST /api/public-name/zeus {handle?, domain?, target?}`, mainnet only): Allod creates a receive-only NWC connection for the target ("ZEUS Pay Lightning address", with the account's name for an account: `make_invoice`, `lookup_invoice`, `get_info`; no pay methods, so a leaked string cannot spend), then `POST /api/lnurl/create {…, nwc_string, address_type: "nwc"}` → `{handle, domain}`. If the owner chose a name, `POST /api/lnurl/update {…, updates: {handle, domain}}` renames it; a refused name keeps the address under ZEUS's name and says so. If create fails, the connection is deleted. Domains: `zeuspay.com`, `zeusnuts.com`.
- **ZEUS Pay+** (choosing the name or domain needs it; without it the address keeps ZEUS's random name): `POST /api/lnurl/status` reports `plusAnnualFeeSats` and `plusExpiresAt` (ISO time or milliseconds; stored as UNIX seconds), refreshed when the address is made, on "Refresh", and every 12 h. `POST /api/public-name/zeus/plus {max_price_sats}` buys or extends a year: Allod reads the price, refuses above the cap, gets a BOLT11 invoice from `POST /api/plus/order` (signed), checks it is BOLT11 for exactly that price on the right network and not expired, pays it from Main through the normal wallet path (§6–7), waits for settlement, and polls the status until the new end date shows. One purchase at a time. `PUT /api/public-name/zeus/plus {auto_renew, max_price_sats}`: opt-in automatic renewal, which needs a cap; the background task renews from 3 days before the end, at most one attempt every 6 h, and records a failure (price above the cap, payment failed) in `plus_error` for the UI.
- **UI choice** (add dialog → LNURL → ZEUS Pay): "Free, with a random name" (no input) or "Choose your name (ZEUS Pay+, paid yearly)" (name + domain). The paid path gets the free address first, because ZEUS reports the price only for an existing address, then shows the exact yearly price and pays only after the owner confirms, then renames; "Keep the free name" stops there. An existing free address is upgraded from its row in the list.
- **Rename**: `PUT /api/public-name/zeus {handle, domain}`. **Remove**: `DELETE /api/public-name/zeus` sends `POST /api/lnurl/delete` and revokes the connection; with `?force=true` an unreachable ZEUS Pay does not stop the local removal (without the connection the address cannot get invoices).
- **Live check**: `verified` once `https://{domain}/.well-known/lnurlp/{handle}` answers a `payRequest`; checked on the same schedule as registry names (§12.3).
- In the UI it is an LNURL address (badge `LNURL`), so Receive shows it without `₿` and without the BIP-353-only notice, first when it is live.
- Trust: ZEUS Pay sees amounts and comments, can answer with its own invoices instead of the node's, and could change its API. The node must be online when someone pays. `ALLOD_ZEUS_PAY_URL` (hidden, testing) points at a stand-in and lifts the mainnet restriction.

### 12.6 Alby addresses (LNURL set up by the owner)

The owner can link Lightning addresses from getalby.com: one per Alby account, so several Alby accounts give several addresses, each paying into Main or an account. Alby needs an account (email and captcha) and has no API for creating addresses, so the owner sets each one up there by hand.

- **Connection**: `POST /api/public-name/alby/connection {target?, connection_id?}` → `{uri, connection_id}` creates a receive-only NWC connection for that address (methods `make_invoice`, `lookup_invoice`, `list_transactions`, `get_info`; metadata `allod_kind = "alby"` and, for an account, `account = <id>`), or with `connection_id` a new key for an existing one (the old string stops working; Allod keeps no connection secrets). The UI shows the string once.
- **Whose invoices**: a connection made for an account makes that account's invoices: `make_invoice` creates the invoice with `app_id` = the account (its balance and history) and `via_app_id` = the connection (migration 0016), and the connection sees exactly the rows it created (`lookup_invoice`, `list_transactions`). A connection for Main makes Main's invoices (not isolated) and also sees only its own rows (`own_rows_only`).
- **Setup on getalby.com**: the owner connects a wallet there with the string (Nostr Wallet Connect) and picks the address.
- **Linking**: `POST /api/public-name/alby {connection_id, address}` asks the address for a test invoice (LNURL) and keeps it only if this node made it, through that connection: its invoice belongs to the chosen account. An address can be linked once. It is then shown as an LNURL address of that account; Allod rechecks unverified ones with the other names. An unfinished one is finished from its dialog.
- **Removing**: `DELETE /api/public-name/alby/{connection_id}` forgets it and revokes its connection (the Alby account stays).
- **Older single address**: `config.public_name.alby` (Main's) is moved into the list on load.
- Mainnet only; on regtest (and in tests) plain-http LNURL services and any network are allowed. Any LNURL service that requests invoices over NWC works the same way.
- Trust: like ZEUS Pay, Alby sees every payment and could answer with its own invoices.

### 12.7 Gateways (TLS passthrough over WireGuard)

A **gateway** is a small public server that gives domains a fixed public IPv4 while the node, its keys and its certificates stay at home. It passes the encrypted HTTPS connection through to the node without decrypting it, so it cannot read payments or swap invoices; it can only block them. It reaches home over WireGuard, opened from home (outbound only: no port forward, works behind CGNAT).

**Table** `gateways (id, label, provider, ipv4, ipv6, mode, tunnel_index, state, provider_ref, expires_at, auto_renew, renew_max_sats, last_handshake_at, checked_at, error, created_at)`:
- `provider`: `manual` (the owner's server) or `lnvps` (rented and set up by Allod, §12.8).
- `mode`: `dedicated` (all of the server's port 443 goes home; no per-domain routes, so domain changes need nothing on the server) or `shared` (port 443 is also used by other sites: routed by SNI with `allod-relay`, one route per domain).
- **Tunnel identity** (derived from the gateway's WireGuard public key `P`, so several Allod installations can share one server): `h = SHA-256(P)`; id = hex of `h[0..3]`, interface and file names `allod-<id>` on the server; UDP port `51821 + (h[3..5] % 4000)` (never 51820, so a VPN on the server keeps it); addresses `10.(64 + h[5] % 64).h[6].1` (gateway) and `.2` (home). A new gateway's key is drawn again while its port, addresses or id would collide with another gateway of the same installation. (`tunnel_index` only numbers gateways.)
- `state`: `setup` (waiting for the first WireGuard handshake), `active`, `offline` (no handshake for 5 minutes), `expired` (LNVPS not renewed), `error`.
- Secrets: `wireguard_home` (home private key, one for all gateways), `gateway_<id>_wg` (the gateway's private key, generated by Allod and put into its setup script), `gateway_ssh` (ed25519, for gateways Allod sets up over SSH).

**Home side**: the `tunnel` Compose service (profile `https`, `deploy/tunnel/`, alpine + wireguard-tools + socat, `cap_add: NET_ADMIN`) shares the `tunnel` volume with Allod. Allod writes `wg0.conf` (interface `<home>/24` for each gateway, one peer per gateway: its public key, `Endpoint ipv4:<port>`, `AllowedIPs <gateway>/32`, `PersistentKeepalive 25`). The service applies changes (`wg syncconf`), forwards `tcp/443` arriving on the tunnel to `caddy:443`, and writes `status.json` (per peer: public key, latest handshake, transfer) every 10 s, which Allod reads for `last_handshake_at` and `state`.

**Gateway side** (setup script from `GET /api/gateways/{id}/setup`, `text/x-shellscript`, run as root on Debian/Ubuntu; idempotent): installs `wireguard-tools`; writes `/etc/wireguard/allod-<id>.conf` (`ListenPort` as above; `Address <gateway>/24`, the gateway private key, peer = home public key with `AllowedIPs <home>/32`) and enables `wg-quick@allod-<id>`; a gateway set up by an older version (interface `allod` with this gateway's key, and the single `allod-gateway-sync` timer) is removed first; the script stops when the port is taken by something else; opens that UDP port. Then:
- `dedicated`: enables IPv4 forwarding and adds nftables rules (table `allod_<id>`) that DNAT `tcp dport 443` to `<home>:443` and masquerade it out of the tunnel (a oneshot unit `allod-gateway-<id>.service` loads them at boot).
- `shared`: installs `allod-relay` (§12.7.1) and runs `allod-relay route add <domain> <home>:443` for each domain on the gateway. It also installs `allod-gateway-sync` with a one-minute systemd timer: it fetches `http://<home>:8089/<id>` over the tunnel (the domain list Allod writes to the tunnel volume's `routes/<id>`, served by the `tunnel` service) and adds or removes routes to `<home>:443` to match; units and files carry the id (`allod-gateway-sync-<id>.timer`). When the tunnel is down the routes stay. So domains added to or removed from the gateway later need nothing run on the server.

**Domains**: a domain served through a gateway gets an A record with the gateway's IPv4 (by its DNS provider, or listed for manual DNS). Caddy at home holds the certificate (TLS-ALPN-01 through the passthrough). A `CAA` record with `accounturi` pins issuance to the home ACME account, so the gateway cannot get a certificate for the domain; the domain page shows it once Caddy has an account. The DNS host and the registrar remain trusted: they can change the A record (and the CAA record), get a certificate for the domain elsewhere and answer LNURL with their own invoices, which payers cannot tell apart; for BIP-353 they can only swap the TXT record publicly. Allod detects such changes (its record checks, §12.1) but cannot prevent them; the UI's shield signal ("who could swap your invoices") leaves the DNS host and registrar out and rates the third party in the payment path: green for an own domain (BIP-353 or LNURL: no third party answers payers), amber for registries (twelve.cash, clank.money: they could swap the offer, but only publicly in a signed record that Allod checks), red for hosted LNURL services (they answer every payer and could swap quietly) and for a node on a rented server (the hosting provider controls it). Of the two own-domain variants, BIP-353 only still makes any DNS-level swap public for everyone, while LNURL lets a hijacked domain serve swapped invoices to chosen payers only.

**Health**: `active` needs a WireGuard handshake in the last 3 minutes; each served domain is checked for HTTPS reachability (§12.1). Shown on the gateway and its domains.

**API** (owner session): `POST /api/gateways {label?, provider: "manual", ipv4, mode}`; `PATCH /api/gateways/{id} {label?, ipv4?, mode?, auto_renew?, renew_max_sats?}`; `DELETE /api/gateways/{id}` (refused while domains use it; an LNVPS gateway stops renewing); `GET /api/gateways/{id}/setup` (the script; owner only).

**Settings → Gateways** (UI): the list of gateways (label, IPv4, provider, tunnel state with last handshake, domains served, expiry for LNVPS); "+ Add gateway": your own server (manual: IPv4, dedicated or shared, then the setup script with copy/download and the one-line `ssh … 'sudo sh -s' < allod-gateway.sh`) or LNVPS (§12.8). A **Gateway providers** section lists LNVPS (account state) and explains manual setup.

#### 12.7.1 `allod-relay` (shared servers)

`deploy/relay/allod-relay` (POSIX sh, root) manages nginx's `stream` module for a server whose port 443 also serves other sites: `install [--default HOST:PORT|none]`, `route add|remove|list DOMAIN HOST:PORT`, `default HOST:PORT|none`, `status`. Port 443 is routed by SNI (`ssl_preread`, `map … hostnames`); other names go to the default (dropped when `none`). Files: `/etc/nginx/allod-relay/{routes.map,default,stream-443.conf}`; on a fresh server also `modules-enabled/98-allod-relay.conf`. A server with its own stream block on 443 includes `stream-443.conf` in it instead. Every change is checked with `nginx -t`, rolled back on failure, and applied with a reload.

Home: Caddy (the `https` profile) holds the certificates; `CADDY_HTTP_PORT`/`CADDY_HTTPS_PORT` choose its host ports (rootless Docker cannot bind below 1024).

### 12.8 LNVPS gateways (rented and set up by Allod)

Allod rents a gateway at LNVPS (`https://api.lnvps.net`), pays it from Main over Lightning, sets it up over SSH, and renews it.

- **Account**: NIP-98 (`Authorization: Nostr <base64 kind 27235>`, a fresh event per request, `u` = full URL, `method`, `payload` = SHA-256 of the body for POST/PATCH), signed by the gateway account key `m/1237'/5'/0'`. LNVPS requires a verified email before ordering. Allod verifies it by itself with the account key's own address `npub…@uid.ovh` (`domains::mailbox`): it publishes the key's DM relay list (kind 10050: `wss://relay.nmail.li`, `wss://auth.nostr1.com`) and a NIP-65 list (also to indexers), sets the email (`PATCH /api/v1/account {email}`), reads the mail the uid.ovh bridge delivers as NIP-59 gift wraps (kind 1059 around a kind 1301 rumor whose content is the raw RFC 822 message; quoted-printable and base64 bodies decoded), takes the `token` of LNVPS's verification link and calls `GET /api/v1/account/verify-email?token=…`. `GET /api/v1/account` (`email_verified`) tells when it is done. The owner may give LNVPS another address instead (`PUT /api/gateways/lnvps/email`) and click the link themselves.
- **Offer**: the cheapest monthly configuration from `GET /api/v1/vm/templates` (standard templates, or the custom template priced with `POST /api/v1/vm/custom-template/price` at 1 CPU, 1 GiB, the smallest disk, 1 IPv4), and the latest Ubuntu LTS from `GET /api/v1/image`. The quote includes the account's VAT (`GET /api/v1/account` → `tax[].rate` for the template's company) and the Lightning processing fee (`GET /api/v1/payment/methods` → `processing_fee_rate`), so the UI shows the total in sats and in LNVPS's currency (e.g. EUR) before buying. An invoice is `amount + tax + processing_fee` (msat); Allod checks the bolt11 amount equals that sum.
- **Cap**: a maximum in a fiat currency (default 10 EUR, VAT and fees included), converted to sats with the owner's rate source (`fiat.source`) when each invoice arrives, for the first payment and renewals; without a rate Allod does not pay. A sats cap (`max_sats`, `renew_max_sats`) is still accepted by the API.
- **Buying** (owner confirms the cap): register the `gateway_ssh` public key (`POST /api/v1/ssh-key`), create the VM (`POST /api/v1/vm {template_id, image_id, ssh_key_id}` or the custom-template route), get the invoice (`GET /api/v1/vm/{id}/renew?method=lightning&intervals=1`), check its amount against the maximum and pay it from Main (a normal payment: §6–7 rules apply; nothing is marked paid without the backend). Allod then polls `GET /api/v1/payment/{id}` and `GET /api/v1/vm/{id}` until the VM runs with an IPv4, connects over SSH as the image's `default_username` (host key pinned from the VM's `host_ssh_keys` when given, else trust on first use) and runs the setup script (`dedicated`) with `sudo`. The gateway becomes `setup`, then `active` on the first handshake.
- **Renewal**: with `auto_renew`, 3 days before `expires_at` Allod gets a renewal invoice and pays it if it is at most `renew_max_sats`; otherwise (or on failure) it shows the gateway as expiring and retries daily. An expired gateway stops serving; its domains show it.
- **Removing**: stops renewal and forgets the gateway (LNVPS deletes the VM 7 days after expiry); its domains must be moved first.
- Progress and errors are kept on the gateway row (`state`, `error`), so a restart resumes where it stopped (an unpaid VM is paid once; a paid VM is set up once).

### 12.2 LNURL proxy over NWC (optional)

A small public server answers LNURL for nodes that keep their IP private. It holds no funds and no node keys: it asks the node for each invoice over Nostr (NWC), the same outbound-only path Allod already uses, so the node needs no inbound port and its address is never sent to the proxy.

- **Binary**: `allod lnurl-proxy` (built from `allod-lnurl`), run on any small VPS behind its own Caddy with on-demand TLS. Config: `--domain` (the proxy's own name, optional), `--relays`, `--db` (SQLite). It stores registrations only: no invoices, no payment history beyond 24 h of request logs for abuse limits.
- **Names served**: usernames under the proxy's own domain (`alice@proxy.example`), and owner names pointed at it (`alice@pay.example.com` with exposure `proxy`). For an owner name, the proxy requires the TXT record `_allod-proxy.<name>` = the registration pubkey (the owner adds it; the UI shows it), and its TLS `ask` answers 200 only for names that are registered and verified. This stops one owner from claiming another's name.
- **Registration** (Settings → Lightning addresses → domain → exposure `proxy`, proxy URL prefilled from a short list in the UI or typed): for each account with a Lightning address, Allod
  1. creates a receive-only NWC connection for that account (`kind='connection'`, child of the account so payments credit its ledger, §10; methods `get_info`, `make_invoice`, `lookup_invoice`; no budget; no pay methods),
  2. sends `POST {proxy}/api/v1/registrations` with NIP-98 signed by a registration key `m/1237'/4'/{app_id}'`, body `{name, username, nwc_uri, node_pubkey, zap_pubkey, nip05_pubkey?, min_sendable, max_sendable, comment_allowed, offer?}`. The proxy checks the TXT record (owner names) or that the username is free (proxy domain), and calls `get_info` over the new connection before answering `201`.
  `PATCH` and `DELETE` on `/api/v1/registrations/{name}/{username}` use the same NIP-98 key. Closing an account or leaving `proxy` deletes the registration and revokes the connection.
- **LNURL-pay** (LUD-06/16, LUD-12 comments, LUD-18 payer data): `GET /.well-known/lnurlp/{username}` (by `Host`) returns the payRequest with `allowsNostr: true` and `nostrPubkey` = the node's zap key. The callback validates the amount and comment, builds the metadata string, and sends NWC `make_invoice {amount, description_hash, metadata: {lnurl_metadata, comment?, payer_data?, nostr?}}`. `nostr` is the zap request when the payer sent one, and `description_hash` is then its SHA-256 (NIP-57), otherwise the SHA-256 of the metadata string. Allod accepts `make_invoice` with both `description_hash` and `metadata` from such connections, checks the hash against the zap request or metadata it was given, validates a zap request as in §12 (kind 9734, amount match), stores the metadata on the transaction (counterparty, §17 M8 notes), and returns the invoice. The proxy answers `{pr, routes: []}` only after checking that the invoice's amount and description hash match and that its payee is `node_pubkey`, so a relay cannot swap invoices. NWC timeout: 10 s, answered as LNURL `ERROR` "node offline".
- **Zaps**: Allod, not the proxy, publishes the kind 9735 receipt when the invoice settles, signed with its zap key (§12) and sent to the relays in the zap request. The proxy needs no Nostr signing key.
- **LUD-21 verify**: the payRequest callback response includes `verify`; the proxy answers it through NWC `lookup_invoice`.
- **NIP-05**: `/.well-known/nostr.json?name=` from `nip05_pubkey` when registered.
- **BIP-353 on the proxy domain**: the operator publishes `<username>.user._bitcoin-payment.<domain>` from the registered `offer` (through their DNS provider; not specified yet), so `alice@proxy.example` works for BOLT12 wallets too. Owner names keep their records in their own zone.
- **Status**: `GET /api/v1/registrations/{name}/{username}/status` (NIP-98) reports DNS check, certificate, and the time of the last successful NWC round trip. Allod shows it as reachability for `proxy`.
- **Limits**: per username 30 callback requests a minute, per client IP 120 a minute, at most 100 registrations per `node_pubkey`; an offline node is answered from a 60 s negative cache so the proxy does not flood relays.
- **Trust** (shown before choosing `proxy`): the proxy cannot spend funds, read balances, or learn the node's IP. It sees payment amounts, comments and payer data, and a dishonest operator could answer with its own invoices instead of the node's and keep those payments (payer wallets do not check the payee). Self-hosting removes that risk; a proxy run by the Allod project is opt-in and named as such.

---

## 13. Web UI (`web/`)

### 13.1 Stack
React 19 + TypeScript 5 (strict), Vite, Tailwind CSS 4, shadcn/ui (Radix) restyled with Allod tokens, Phosphor icons (`@phosphor-icons/react`, not lucide), React Router, `nostr-tools` for NIP-07/NIP-46/NIP-98, SWR for data fetching (as in HUB), `zod` for forms with `react-hook-form`, `qrcode.react`, `@getalby/bitcoin-connect`-free (no Alby deps). Typed API client generated from `openapi.yaml`. Light/dark themes. Mobile-first responsive layout. Vitest + Testing Library; Playwright for e2e against the regtest stack.

Build output goes to `web/dist`, which is embedded with `rust-embed` (`--features embed-ui`, on by default in release). In dev, Vite proxies `/api` to `127.0.0.1:8029`.

### 13.2 Visual identity
Polished and friendly like Alby Hub, but recognisably its own product. Same building blocks (shadcn/ui), different look:
- **Layout:** a top bar with primary navigation (Home, Accounts, Connections, Node, Settings) instead of a left sidebar. No account switcher in the top bar. Send and Receive choose an account in their forms (Main by default); Activity starts with all accounts and offers a local account filter. On mobile, a bottom tab bar. Home is an account-centred overview, not an app grid.
- **Typography:** Space Grotesk for headings, Inter for body text, IBM Plex Mono for amounts, keys and hashes. The wordmark is lowercase "allod" in the custom "Allod Geometric" font (glyphs a, l, o, d only; `web/src/assets/fonts/`, `android/.../res/font/`, the ISO overlay), no extra letter spacing (the font carries its own), `#F4F2EB` on dark backgrounds.
- **Colour:** deep ink/graphite neutrals with a single **teal accent** for actions (`#007B76`, the logo's mid teal, in light mode; `#2dd4bf` in dark mode), and Bitcoin orange reserved for amounts and on-chain items. No Alby yellow.
- **Shape:** 12 px radii on cards, pill-shaped primary buttons, soft gradient hero for balances, subtle grain texture in dark mode.
- **Icons:** Phosphor (duotone weight for navigation, regular weight inline). Custom glyphs for Lightning, BOLT12 offers and on-chain.
- **Logo:** a cream geometric "A" with a Bitcoin-orange dot at its peak on a teal radial gradient (`#009F96` → `#007B76` → `#004C49`) rounded square. The source is `web/public/favicon.svg`; every other copy (web, Android, StartOS, ISO) is derived from it.
- **Illustrations:** abstract generative patterns derived from the account pubkey (identicon-like "account sigils"), no stock art and no mascot.
- **Not included:** no app store, no partner app grid, no Alby account or Alby services, no "Alby Go" branding anywhere.
- **Amounts and fiat:** sats by default; BIP-177 `₿` format optional. Fiat display is supported: USD by default (the setup wizard does not ask), changeable in Settings to another currency or "none". Rates come from a configurable source (default yadio, alternatives selectable), fetched by the node. Settings says that fetching rates contacts that source. "None" makes the UI fully offline.

### 13.3 Screens (HUB `frontend/src/screens` for UX reference)
Onboarding:
1. Welcome → **connect your Nostr signer** (extension or bunker; becomes the owner) → (standalone only) connect ldk-server (address, cert upload or paste, macaroon paste; live "test connection" showing node alias, network and missing permissions) → create or restore the recovery phrase → **backup confirm** (re-enter 3 random words, checked by `POST /api/backup/verify`; skippable only when `skip_backup_check` is set, §3.1) → display currency → done, with Bitcoin sync progress while the node catches up.
1a. **Unverified backup.** While `GET /api/backup` says `verified: false`, every page shows a warning banner linking to **Back up** (`/backup`: sign to reveal the phrase via `POST /api/recovery-phrase`, show the words, re-enter 3). Receive adds a stronger warning on the On-chain tab and when the "Just-in-time channel" box is ticked, since those funds land on this node and only the phrase recovers them.
2. Sign in with Nostr (sessions end when Allod restarts). A short trust-model note in setup explains that the node trusts the machine it runs on (§3.3).

Main (top bar: Home, Accounts, Connections, Node, Settings):
3. **Home**: total / Main / accounts balances, Send, Receive, recent transactions (live via SSE), onboarding checklist (open first channel, back up mnemonic, create first connection).
4. **Send**: a single input (paste or scan QR: bolt11, offer, BIP-321, LN address, BIP-353, LNURL) → decode preview → amount (if needed) + optional payer note (only shown for BOLT12) + max fee (advanced) → pay from: Main or an account → result (preimage, fee, payer proof copy for BOLT12).
4b. **Lightning addresses** (Settings): the list of addresses; "+ Add" is a wizard of choices, one per step, with Back: (1) **where**, two blocks side by side: **your own domain**, holding the own domains (each with its live state and how it is reached) and **add a new domain** (just its name; its DNS provider is set in Domains), and **a third-party service**; (2) **which wallets**: both, LNURL only or BIP-353 only ("both" is not offered for third parties: no service gives both on one name); (3) when LNURL is wanted on a domain not served yet, **how payers reach it**: one of the gateways (home IP hidden; a link to Gateways when there is none) or the home IP (with the warning); for third parties, **the service** among those giving the choice (twelve.cash free random name, twelve.cash chosen name, clank.money; ZEUS Pay, Alby); (4) **the account** it pays into (skipped when Main is the only one; accounts that already have an address of that service are disabled; ZEUS Pay is Main only); (5) the last step: on an own domain the **username**, suggested from the account (`sats` for Main, the account name as a slug otherwise, empty when taken on that domain); for twelve.cash and clank.money the name (same suggestion, checked with the service); for Alby the connection steps, asking only the username (`@getalby.com` is fixed). An own-domain address's dialog changes the same choice later (both / LNURL only / BIP-353 only: `PATCH /api/addresses/{id} {lnurl, bip353}`, migration 0015 adds `bip353`, default on; an LNURL-only address has no TXT record). The earlier questions flow and its stored answers (`/api/public-name/prefs`) are no longer used by the UI.
5. **Receive**: tabs "Address" (first: the QR code and text of the Lightning address of the wallet being received into, opened by default when it has a live one: own-domain record seen in DNS, or a registry name published with an active offer; with several, buttons pick one. An address that is live through BIP-353 only (every address until LNURL serving exists) is written `₿user@domain` (the QR code carries the bare `user@domain`), with a notice that only BIP-353 + BOLT12 wallets can pay it, that LNURL is not active, and a link to Settings → Lightning addresses. Without a live address the tab is greyed out but selectable, and explains what a Lightning address is, lists addresses that are not live yet with the reason, and links to Settings → Lightning addresses), "Invoice" (amount, description, expiry; "Just-in-time channel" checkbox, shown only when the node reports `jit_receive`, explaining that the LSP opens the channel when the invoice is paid and the payer pays its fee — requires an amount), "Offer" (reusable; amount optional; shows existing offers list with copy/QR/disable), "Unified" (BIP-321 with both), "On-chain" (address, with a "new address" button). Live "paid!" state via SSE.
6. **Transactions**: filterable list (direction, state, type, app), detail drawer (all fields, labels editable, raw JSON toggle).
7. **Connections**: list (name, last used, budget bar, isolated badge, balance if isolated, expiry); **New connection** (presets: "Read-only", "Payments with budget", "Full access", "Custom"; scope checkboxes; budget amount + renewal; expiry presets 1 week/1 month/1 year/never; isolated toggle; relay override; advanced `superuser` toggle granting `create_connection` (§8.4 #13) — not a regular scope checkbox, always shown with a danger warning that child connections spend from this wallet and a parent budget does not cap their combined spending) → shows the connection URI + QR **once** with copy and a warning; **Connection detail** (edit incl. the `superuser` toggle, revoke, the same warning while the scope is granted, request log from `request_events` (method + state + time), transactions for isolated); **Authorize** page for NWC-08 (§8.7) with the app name/icon, requested methods in plain language, budget, expiry, and approve/decline.
8. **Accounts**: overview of Main plus every account (balance, last used) → create (name, optional lightning address) → created page (connection URI/QR, lightning address, BIP-353 record, short "use it in any NWC wallet" tips without naming one vendor) → detail (balance, statement, top-up, withdraw, regenerate, close).
9. **Node**: overview (alias, pubkey, network, block height, synced), **Channels** (list with inbound/outbound bars, state, fees; open channel form with peer pubkey@host:port, amount, public toggle; close (coop/force with a strong warning); edit fees; splice in/out), **Peers** (list, connect, disconnect), **On-chain** (balance, transactions, send, bump fee), **Forwards** (list, fees earned), **Sign message**. Overview and Channels show a warning when spendable on-chain sats < 25 000 × number of channels: the node may be unable to raise the closing fee for an urgent close (§2.3.2).
10. **Settings**, grouped in sections (a section list beside the content on wide screens, stacked on mobile) so new settings slot into an existing group: general (display unit, theme, fiat currency and rate source; the current choice is always shown, not only highlighted in the list), Bitcoin (chain source: stack default, own Bitcoin Core, Electrum or Esplora; §2.3.2), notifications (§8.9: enable on this device, every registered device with its event types, hide amounts, send a test, remove; the iOS "Add to Home Screen" hint), relays (add/remove, status), security (owners, show recovery phrase after re-auth, API tokens), backup (export, plus instructions for ldk-server seed backup), LNURL/domain, advanced (settle safety blocks, logs viewer, audit log, debug info, health).

### 13.4 UX rules
- Every irreversible action (force close, close an account with balance, regenerate connection, reveal recovery phrase) needs a typed confirmation or a fresh Nostr signature (§11.1).
- Secrets (connection URI, mnemonic, API token) are shown once, blurred by default, with a "copied" toast, and never logged.
- Optimistic UI is not allowed for money movements. Show pending states from the server.
- All strings go through an i18n layer (`i18next`); English ships first.

---

## 14. CLI (`allod` binary subcommands)

```
allod serve [--config allod.toml]
allod setup --owner npub1... [--mnemonic-file f] [--ldk-address host:port --ldk-cert file --ldk-macaroon file]   # headless setup
allod owner add|list|remove <npub>             # owner keys; the recovery path for a lost key
allod info | balance | channels | peers
allod pay <payment> [--amount-msat N] [--max-fee-msat N] [--note TEXT] [--from-app ID]
allod receive [--amount-msat N] [--description TEXT] [--offer] [--app ID]
allod apps list | create --name N --scopes a,b --budget-msat N --renewal monthly [--isolated] | delete ID
allod accounts list | create NAME | topup ID AMT | withdraw ID AMT
allod tx list [--app ID] | show ID
allod token create --name N --readonly
```
The CLI is a thin client of the REST API (`--url`, `--token` / `ALLOD_TOKEN`). Output is a table by default, or `--json`.

---

## 15. Security

- Secrets in memory are wrapped in `Zeroizing`. Secrets are never logged: add a `tracing` layer test that greps logs for known secret values in e2e.
- Request content is never persisted (only the method) and invoices are logged only at debug level. Log levels: default `info` logs carry no amounts tied to app names. Privacy over observability.
- All Nostr keys are per app. Rotating a connection = new app key + new wallet key.
- Rate limits: public challenge and login routes allow 30 requests/min per TCP peer and 300/min globally per route; public setup allows 10/min per peer and 100/min globally. Proxy headers are not trusted as a source identity. Login challenges (§11.1), NWC per app (§8.2), API 300 req/min per token.
- Audit retention: on startup and hourly, delete audit rows older than 30 days and oldest rows beyond 100,000 rows or 32 MiB of stored JSON text. Delete in batches of 5,000. Log a warning when rows or JSON text reach 80% of the budget, or when pruning fails. Repeated malformed login requests create no audit rows.
- HTTP server: add `Content-Security-Policy` (self only, except `connect-src 'self' wss: ws:` so the UI can reach NIP-46 remote signers' relays), `X-Frame-Options: DENY` **except** `/connections/new` (NWC-08 popup: allow framing only when `ALLOD_ALLOW_FRAME_ANCESTORS` is set), `Referrer-Policy: no-referrer`.
- DB file permissions are 0600. Data dir 0700.
- Postgres: TLS required unless the host is localhost.
- ldk-server macaroon: recommend a dedicated macaroon with the permission list in §3.2 rather than admin. The setup screen shows a `ldk-server-cli create-macaroon` command for it.
- Supply chain: `cargo deny` (licenses, advisories) and `pnpm audit` in CI. Lockfiles committed.
- Threat model doc (`docs/threat-model.md`, M10): a malicious NWC client (DoS, budget evasion via concurrency, cross-app data leaks), a malicious relay (replay, drop: mitigated by dedupe, 6 h staleness, signatures), local attacker with disk access (can spend funds via ldk-server's plain files: mitigated only by OS disk encryption and physical security, §3.3; the DB alone holds no secrets but does hold transaction history), compromised browser session (token expiry, readonly tokens).

---

## 16. Testing

### 16.1 Unit
- Ledger math (journal entries sum to 0, balances, budgets, period starts across DST/UTC, fee reserve, ledger invariants §6.3) with `proptest`: random sequences of payments never produce a negative isolated balance and never exceed a budget, even with concurrent validation (`tokio::test(flavor="multi_thread")`, 100 concurrent pays against a 10-payment balance → exactly 10 succeed).
- BIP-321 parsing and amount conflict matrix (port HUB `pay_controller_test.go` cases).
- Error mapping for every `CoreError` variant.
- Permission → methods mapping, effective-methods filtering by capabilities.
- NIP-04/NIP-44 encrypt/decrypt round trips against `nostr-sdk` test vectors.

### 16.2 Handler tests (mock backend)
`MockBackend` implementing `LightningBackend` with a scripted payment state machine and an event emitter. Port every HUB `nip47/controllers/*_test.go` case to Rust (names kept in comments for traceability), plus Allod additions:
- hold invoice owned by app A cannot be settled or canceled by app B → `NOT_FOUND`.
- underpaid claimable → auto-fail, no notification.
- BOLT12 incoming to an app's offer credits that app and notifies only it.
- `make_offer` with issuer / single_use on ldk-server → `BAD_REQUEST`.
- `lookup_payment` all forms, `MULTIPLE_MATCHES`, `UNSUPPORTED_PAYMENT_TYPE`, invisible row → `NOT_FOUND`.
- `create_connection` cannot escalate (superuser, budget > remaining, non-isolated from isolated).
- self-payment bolt11 / bolt12 / hold between two isolated apps.
- notifications visibility matrix.
- stale (>6 h) pay and `create_connection` requests dropped; duplicate event id dropped; NIP-40 expiration honored; privileged requests without short NIP-40 expiration rejected; replay cannot restore a child key after revocation.

### 16.3 Isolation property test
For each NWC method, generate data for apps A (isolated), B (isolated), C (non-isolated). Assert that A's responses never contain identifiers from B's rows, and that C's never contain A's or B's.

### 16.4 E2E (regtest, `tests/e2e`, run with `cargo test -p allod-e2e -- --ignored` and in CI nightly)
Harness: the regtest overlay (§2.3.9) on top of the production Compose bundle (bitcoind, Alice/Bob/Carol ldk-server, two relays, Allod). Tests reach services through `ALLOD_E2E_TARGET=compose` and get credentials from `docker/.dev/`. Each test creates its own apps/accounts, so tests are independent and can run against a stack that is already running. Fast handler tests (§16.2) still use `MockBackend` and an in-process `nostr-relay-builder` relay, with no Docker. The full matrix is set by `ALICE_CHAIN` × `ALLOD_DB` (§2.3.9). Scenarios:
1. Setup + Nostr login via API.
2. Open a channel Alice→Bob, mine 6 blocks, check balances.
3. NWC client (`nostr-sdk` `nwc` client) pays Bob's bolt11; check `payment_sent` notification and budget usage.
4. Bob pays an invoice made via NWC `make_invoice`; check `payment_received`.
5. BOLT12: Bob pays an offer from NWC `make_offer` (variable amount) twice → two settled rows on the owning account; Alice pays Bob's offer via `pay` with `payer_note`; `lookup_payment` by offer_id + hash.
6. Hold invoice: make → Bob pays → `hold_invoice_accepted` with `settle_deadline` < real deadline → settle → settled. Second run: cancel → Bob's payment fails.
7. Accounts: two isolated apps; topup; pay each other internally (no LN traffic, checked by Bob's channel balance staying the same); insufficient balance error.
8. Crash recovery: start a payment, kill Allod mid-flight (drop the runtime), restart → reconciler settles the row.
9. Event stream drop: restart ldk-server while payments arrive → reconcile catches them.
10. NWC-08 Nostr flow: a client publishes nothing; Allod approves via API → client receives the 13194 with `state`.
11. Playwright UI smoke: setup, create a connection, send, receive, open a channel.

### 16.5 Conformance
Run the Alby NWC test client (`@getalby/sdk` `nwc` examples) against a regtest Allod as a black-box check for `get_info`, `pay_invoice`, `make_invoice`, `list_transactions`, notifications.

---

## 17. Milestones (each one is a shippable PR series; agents take one milestone at a time)

| M | Scope | Done when |
|---|---|---|
| ~~**M0 Skeleton + deployable stack**~~ | ~~Workspace, crates, CI (fmt, clippy -D warnings, test, cargo-deny), `allod serve` with `/api/info`, `/api/health`, config loading, tracing, SQLite migrations, Vite app shell served embedded. **Deployment foundation (§2.3)**: all images (allod, allod-ldk-server with `ldk-entrypoint.sh` and handoff, allod-bitcoind, allod-tor), production `deploy/compose.yaml` with every chain mode and the profiles `https`, `tor`, `backup`, `postgres`; the regtest overlay + `bootstrap` + `justfile` + `allod-devtools check`; the multi-arch release pipeline (§2.3.8) producing signed images and the Compose bundle. Allod only needs to start and report health in M0; managed-mode setup lands in M1.~~ | ~~CI green; `docker compose -f deploy/compose.yaml up` on signet syncs ldk-server through each chain mode; the regtest overlay ends with funded nodes and active channels; `just doctor` passes for every `ALICE_CHAIN`; a tag dry-run produces signed amd64+arm64 images and the bundle.~~ |
| ~~**M1 Keys, owner login, setup**~~ | ~~Built in steps: (1) `allod-keys` (recovery phrase, BIP32 Nostr keys, backup encryption) and `config` storage; (2) `owners` + NIP-98 Nostr login + in-memory JWT sessions + API tokens; (3) setup API: secrets files, managed-mode seed handoff, scoped ldk-server macaroon, restore with birthday height, sync progress, `SETUP_REQUIRED` guard (§2.3.3–2.3.4, §3.2–3.3); (4) web: new visual identity (§13.2), Nostr signer connect (NIP-07/NIP-46), setup wizard with trust-model note, sign-in screen; `ALLOD_DEV_AUTO_SETUP` (§2.3.9).~~ | ~~Unit tests per step; Playwright setup → sign-in flow against the regtest stack with a test NIP-07 signer; `just up` ends with a set-up Allod.~~ |
| ~~**M2 Backend**~~ | ~~`allod-ln` ldk-server impl, event stream, capabilities, `MockBackend`. Node/channels/peers/on-chain/balances APIs + Node UI screens.~~ | ~~E2E 1–2 pass.~~ |
| ~~**M3 Ledger + payments**~~ | ~~transactions/offers tables, **double-entry ledger** (§6.3: accounts, journal entries, postings, invariants, `allod ledger check/rebuild`), §6–7 complete (bolt11, bolt12, hold, self payments, reconciler), payments/receive APIs, Wallet UI (send/receive/transactions), SSE.~~ | ~~Unit + property tests; e2e 3–9 via API (not NWC yet).~~ |
| ~~**M4 NWC core**~~ | ~~Transport, pipeline, encryption, info events, NIP-47 core methods (get_info, get_balance, get_budget, pay_invoice, make_invoice, lookup_invoice), apps + permissions + budgets, Connections UI.~~ | ~~Handler tests; e2e 3–4 via NWC.~~ |
| ~~**M5 NWC extensions**~~ | ~~02 notifications (outbox), 03 hold, multi_pay_invoice, 05 list_transactions, 06 metadata, 09 lookup_payment, 12 make_offer, 321 pay/receive, create_connection.~~ | ~~All §16.2 tests; e2e 5–6.~~ |
| ~~**M6 Accounts**~~ | ~~§10 complete: API + UI, transfers, statements, isolation tests §16.3.~~ | ~~e2e 7; isolation property test.~~ |
| **M6.5 Distribution (first public release)** | ~~`install.sh`~~ + `allod-stack` helper + cloud-init/one-click templates (§2.3.5); ~~litestream backups~~ with age encryption, restore flow, and restore-epoch guard (§2.3.7); deployment UI (§2.3.4); ~~Umbrel app + StartOS package (§2.3.6)~~; signet smoke tests for every artifact. | Fresh Debian VPS → working mainnet-ready node with HTTPS in < 15 min of user time (plus IBD); Umbrel and StartOS installs pass their platform test checklists; backup/restore e2e on regtest (restore from replica, channel still works; second start from the same backup is refused). |
| ~~**M6.6 Device notifications**~~ | ~~Web Push (§8.9): VAPID key, subscriptions and push outbox, API, service worker, manifest, Settings → Notifications.~~ | ~~RFC 8291 test vector passes; API tests for subscriptions, recipients and response handling; a payment shows a notification in a desktop browser and an installed PWA.~~ |
| **M7 NWC-08** | HTTP + Nostr flows, authorize UI, deep-link docs. | e2e 10. |
| **M8 Receive identity** | ~~allod-lnurl (paying addresses and LNURL is done; see the notes above)~~: receiving via LUD-16, NIP-57 zaps, BIP-353 helper, NIP-05; public name on the owner's domain, exposure `private`/`proxy`/`public`, on-demand TLS and listed BIP-353 records (§12.1); twelve.cash and clank.money names (§12.3, §12.4); optional `allod lnurl-proxy` over NWC (§12.2). ~~JIT receive via `Bolt11ReceiveViaJitChannel` when an LSP is configured **is done** (M4.5 pull-forward: backend trait + capability, wallet/REST/NWC-321/UI, `LDK_LSPS2_LSP*` bundle config, mock tests).~~ | LNURL tests; zap receipt test; public-name tests (BIP-353 records follow account create/close, `tls-ask` answers only the current name; twelve.cash and clank.money against mocks); proxy e2e on regtest: a node with no inbound port receives an LNURL payment and a zap through `allod lnurl-proxy`, the zap receipt is published by Allod, and a swapped invoice (wrong payee) is refused by the proxy. |
| **M9 On-chain NWC** | NWC XX-onchain (and XX-silent-payment if ldk-server supports it) for `lookup_payment` / `list_transactions` / `pay` with on-chain instructions, behind a setting. | Spec tests. |
| **M10 Hardening** | ~~Postgres support + advisory lock (the `db-postgres` profile already exists from M0)~~, threat model, ~~security headers~~, fuzzing (`cargo fuzz` on BIP-321/NWC request parsing), RaspiBlitz/myNode scripts, NixOS module, docs site (`docs/`), release workflow (static musl build, multi-arch Docker image, reproducible web build). | Release candidate. |


**M0 implementation notes (2026-10-28).**
- Repository layout: `deploy/` (production `compose.yaml`, `compose.regtest.yaml`, the CI-only `compose.ci.yaml` build-cache overlay, `regtest.env`, `.env.example`, `Caddyfile`, `litestream.yml`, `scripts/` for bootstrap and miner) and `images/` (Dockerfiles and entrypoints). The `tools` image is a target of `images/allod.Dockerfile`.
- Until M1 adds the setup wizard, the regtest `bootstrap` writes Alice's recovery phrase into the handoff volume (`allod-devtools gen-mnemonic`). On real networks the phrase has to be written by hand for testing.
- `ldk-entrypoint.sh` starts ldk-server only after bitcoind leaves initial block download, and the container sets TCP sysctls (O-12).
- Verified on signet with `prune=5000`: if ldk-server starts during bitcoind's initial block download, it saves an early chain tip; once bitcoind prunes past it, catch-up fails for good (`Block not available (pruned data)`). Waiting for IBD to end is therefore required on pruned nodes, not just an optimization. A fresh node started after IBD syncs to the tip in seconds.
- `just` one-off commands (`alice`, `bob`, `carol`, `btc`, `mine`, `doctor`) use `docker run` with the tools image on the stack network, because `compose run` rebuilds the image on every call.
- rbitcoin mode is deferred. Umbrel/StartOS packages, `install.sh` and backups remain in M6.5.
- Verified on signet (2026-10-28) with `prune=5000` and `loadtxoutset` from a snapshot at height 290,000. The snapshot loaded in about 4 minutes. bitcoind reported `initialblockdownload=false` about 20 minutes later, while background validation was only at block ~152k. ldk-server started at that point, synced to the tip in seconds, and logged no "pruned data" errors across a bitcoind restart during background validation. While background validation runs, bitcoind prunes the snapshot chain harder than the target: it kept only the last ~1,050 blocks, using 2.5 GB on disk. During that phase ldk-server must not stay offline longer than that window. Optional `loadtxoutset` in setup: see "Fast sync" below.

**M2/M3 implementation notes (2026-10-28).**
- `allod-ln` defines `LightningBackend` (async trait) with `LdkServerBackend` and a feature-gated `MockBackend` (`lnbcrtmock…` invoices, `lno1mock…` offers, scripted send outcomes). The API reaches the node through `LdkSource`, so tests swap in the mock.
- Wallet SQL runs on sqlx's `Any` driver with `$N` placeholders for both SQLite and Postgres. Boolean columns are read as `CAST(x AS INTEGER)`, because the `Any` driver cannot decode SQLite booleans.
- Every wallet write goes through one write lock. The `ACCEPTED` transition of a hold invoice and its `settle_deadline` are written in one DB transaction.
- `POST /api/payments` accepts bolt11, BOLT12 offers and BIP-321 URIs. ~~Lightning addresses, BIP-353 and LNURL return `NOT_SUPPORTED` until M8.~~ (done: LNURL in M8, BIP-353 sending 2026-11-07, §11.3)
- Live updates: `POST /api/events/ticket` returns a single-use ticket that is valid for 60 s, for `GET /api/events?ticket=` (SSE). The reason is that EventSource cannot send headers.
- UI: Home (balances, recent activity), Send, Receive (invoice, offer, unified BIP-321, on-chain), Activity (filters, search, detail with labels), and Node (overview, channels, peers, on-chain, forwards, sign/verify message).
- **Not yet implemented from M3** (to finish before M6.5):
  1. CLI `allod ledger check` and `allod ledger rebuild` (§6.3). The check exists (`GET /api/ledger/check`, run at startup and on every reconcile); the rebuild of `ledger_balances` from `postings` does not.
  2. Reconciler §7.6 step 3 fallback (O-1): implemented for pending inbound backend payments; the recovered hold has `settle_deadline = NULL` because ldk-server does not expose the deadline in payment queries.
  3. Reconciler §7.6 step 4 cursor: each run walks up to 50 pages of `list_payments` instead of stopping at `config.reconcile_cursor`.
- BOLT12 offers with an amount and an empty description get the node alias (or "Payment") as description, because BOLT12 requires one.
- e2e: the `allod-e2e` crate runs scenarios 1–9 through the HTTP API against `just up` (`just e2e`). It drives Bob through ldk-server-client and bitcoind through RPC. Scenario 7 covers internal self payments here; isolated accounts follow in M6. Scenarios 8–9 restart containers, so they run one at a time.

**M4 implementation notes (2026-10-28).**
- `allod-nwc`: one `nostr-sdk` client without a signer. Every response and info event is signed with the app's wallet key (`m/1237'/1'/{app_id}'`). Subscriptions are replaced with fresh ids (`allod-nwc-{generation}-{n}`, 100 single-key filters each) after every connection change (1 s debounce), at startup, and every 24 h.
- Methods answered: `get_info`, `get_balance`, `get_budget`, `pay_invoice`, `make_invoice`, `lookup_invoice`. Other methods return `NOT_IMPLEMENTED` and are left out of info events and `get_info.methods` until their milestone (`apps::IMPLEMENTED_METHODS`).
- `pay_invoice` waits up to 5 min for a final state after the wallet returns `PENDING`.
- Budgets (§6.4) are checked in the same locked DB transaction that reserves the funds, for external and self payments. Owner transfers are not budgeted.
- `GET/POST /api/apps`, `GET/PATCH/DELETE /api/apps/:id`, `POST /api/apps/:id/regenerate`. The pairing URI is returned once, and the client secret is never stored.
- Connections UI: list with budget bars, New connection (presets, scopes, budget, expiry, isolated, relays, advanced superuser toggle with risk warning) showing the link once, and a detail page (edit, new link, revoke, request log).
- `allod-db` got a `build.rs`, because `sqlx::migrate!` does not rebuild when a migration file is added.
- **Not yet implemented from M4:**
  1. Requests to an unknown connection are dropped without a reply. §8.2 step 3 replies `UNAUTHORIZED` with the legacy service key, and there are no legacy apps yet.
  2. `/api/health` does not report relays or the NWC handler yet (§3.4).
  3. Graceful shutdown does not wait for in-flight NWC payments (§3.3).
  4. `GET /api/apps` ignores `kind`, `q`, `order`, `limit` and `offset` and returns every connection.
  5. Info events are republished on connection changes, at startup and every 24 h, but not when the backend capabilities change (§8.1).
- `ALLOD_CLIENT_RELAYS` (default: `ALLOD_RELAYS`) sets the relay URLs written into connection links, for clients that reach the relays at other addresses than Allod does. The regtest overlay uses `ws://127.0.0.1:7000,7001`, so links work from the host. `just nwc-uri` creates a test connection and `just nwc "$URI" <method> [params]` calls it (`allod-devtools nwc-uri|nwc`).
- e2e scenarios 3–4 over NWC: `crates/allod-e2e/tests/nwc.rs`. Notifications (`payment_sent`/`payment_received`) are checked in M5.

**M5 implementation notes (2026-10-28).**
- All §8.4 methods are answered except NWC-08 (M7). Info events carry `notifications` (when granted) and `extensions` (`02 03 05 06 09 12 321`, filtered by the app's methods).
- Notifications use a transactional outbox. `store::transition` and `store::insert` queue a row in the same DB transaction whenever a transaction reaches SETTLED (`payment_received` / `payment_sent`) or a hold invoice reaches ACCEPTED (`hold_invoice_accepted`). Rows go to every connection with the `notifications` scope that may see the transaction (§8.5). A worker publishes them (kind 23197, plus kind 23196 for apps whose latest request used NIP-04 or that never sent one; `apps.last_encryption`, migration 0005) and retries up to 10 times. The payload is the NWC-09 record; `hold_invoice_accepted` adds `settle_deadline`.
- `lookup_payment` and `list_transactions` items use the NWC-09 envelope with `details`, and keep the NIP-47 fields for older clients. Preimages are returned only for settled rows whose SHA-256 matches the payment hash, so an unpaid invoice's preimage is never sent.
- `make_hold_invoice` with `min_cltv_expiry_delta` returns `BAD_REQUEST` (O-4). `make_offer` with `issuer` or `single_use` returns `BAD_REQUEST` (O-2, O-3). `get_budget` returns `{}` without the `pay_invoice` scope, as in Hub.
- `create_connection` validates methods and notification types, never grants `create_connection`, keeps isolated callers to isolated children, and when the caller has a budget requires `max_amount` ≤ its remaining budget (`QUOTA_EXCEEDED` otherwise). It resubscribes before replying.
- Request log retention: an hourly job deletes `request_events` older than 7 days, except each connection's newest 50 (its request log), in batches of 5,000; `response_events` follow through `ON DELETE CASCADE`. Dedupe covers the full 6-hour replay window; privileged requests additionally expire within 10 minutes. Stored rows hold no request content, unlike Alby Hub's `ContentData`; Hub keeps the newest 1,000 events overall.
- The NWC service subscribes before it publishes info events, and waits for relay connections only when a relay was added. Requests are ephemeral events, so one sent before the subscription exists is lost.
- Tests: `crates/allod-nwc/tests/{nwc,nwc_ext,hub_port,isolation}.rs`. `hub_port.rs` gives every non-keysend Alby Hub controller case a counterpart and names the Hub test it follows. Hub refuses isolated connections on backends that cannot isolate; Allod isolates in its own ledger, so the counterpart asserts that isolated creation works. `isolation.rs` is the §16.3 property test: 4 random data sets, every method called by apps A and B (isolated) and C (not), including lookups, cancels and offer lookups with the other apps' identifiers. No response or notification may contain them.
- Hub hold-invoice fund-loss notes (HUB-NOTES), checked against Allod:
  1. `settle_deadline` is LDK's claim deadline minus `settle_safety_blocks` (3). Tested.
  2. Only the owning app can settle or cancel a hold; other apps get `NOT_FOUND`. Tested.
  3. A hold the node fails on its own (inbound `PaymentFailed`, or found failed on reconcile) becomes CANCELED with `failure_reason = 'canceled by the node'`, never staying "accepted". Allod also cancels holds itself at `settle_deadline`. Tested (`hold_failed_by_the_node_is_not_reported_accepted`).
  4. Bolt11 expiry only moves PENDING rows to EXPIRED; an accepted hold stays "accepted". Tested (`accepted_hold_does_not_expire_with_its_invoice`).
- Relays rate-limit, typically around 60 events a minute. Info events are republished only when their content changed (and all of them by the daily refresh), not for every connection on every change. Responses and info events are retried with exponential backoff for about 30 s while every relay refuses them. Notifications retry through the outbox.
- A node without BOLT12 pays the BIP-321 URI's `lightning` invoice when there is one. Otherwise `pay` answers `UNSUPPORTED_PAYMENT_INSTRUCTION` (and `make_offer` is not advertised).
- `pay` with `max_fee` passes the cap to ldk-server as `max_total_routing_fee_msat`. LDK only searches routes within that cap, so a `RouteNotFound` failure under a cap is answered `FEE_LIMIT_EXCEEDED`: no payment was attempted, and the reserve is released. Without a cap, the same failure is `PAYMENT_FAILED`.
- Failure reasons: when polling sees a payment fail before the `PaymentFailed` event, the event gets up to 1 s to record its reason. Reasons from events that arrive before the row knows its payment id (`early_failures`) are kept for polling to use.
- The mock backend models mainnet invoices (`lnbcmock…`), expired invoices, mainnet offers (`lno1mockmain…`) and expired offers (`lno1mockexp…`), and gives each BOLT12 payment its own hash. It also records the fee cap it received. `hub_port.rs` uses these for Hub's BOLT12 wrong-network, expiry and fee-limit cases, and renames a table to test the database-error case.

**M6 implementation notes (2026-11-02).**
- Accounts are apps with `kind='account'` (`POST /api/apps {kind: "account"}` works too): isolated, scopes fixed to the §10 list, budget renewal `never`, metadata `{"allod_kind":"account"}`. NWC answers them like connections: the request lookup, the subscriptions and the notification outbox take every app that is not closed (until M6 they took connections only).
- Closing is soft (`apps.closed_at`, migration 0008); see §10. Hard-deleting an app whose `wallet:app:<id>` ledger account exists would hit the `ledger_accounts.app_id` foreign key, and `ON DELETE SET NULL` on `transactions.app_id` would move its history into Main.
- `GET /api/accounts/summary.total_balance_msat` sums accounts only (`/api/balances.accounts_total_msat` also counts isolated connections). The coverage warning compares the accounts total with Lightning spendable on Accounts and Home.
- `lud16_username` is validated (§12 rules plus a reserved list) and reserved. With a username, a public URL that is a domain, and BOLT12, creation also makes a variable-amount offer owned by the account and returns the BIP-353 TXT record. The Lightning address itself answers once LUD-16 receiving lands (M8).
- UI: Accounts (Main plus accounts, coverage warning, linked NIP-05 and Lightning names), New account (link shown once, blurred until revealed, optional NIP-05 and Lightning address on chosen domains, BIP-353 record), account detail (balance, top-up, withdraw, statement, rename, new link and close behind a typed confirmation; closing with funds signs the sweep with the Nostr signer). Send and Receive choose their account locally, starting at Main. Activity starts with all accounts and can filter to one account. Home lists the accounts. Unified receive (`/api/receive`) still pays into Main.
- `GET /api/offers?app_id=` filters by owner (`main` or an id).
- Tests: `allod-api/tests/accounts_flow.rs` (routes, re-auth for sweeps), `wallet::tests::closing_accounts` (in-flight refusal, sweep, disabled offers, late payment after closing, running balances), `allod-nwc/tests/accounts.rs` (two accounts pay each other with the node set to fail every send, notifications, a closed account goes silent), the §16.3 isolation property test with app A as an account, and e2e scenario 7 (`allod-e2e/tests/accounts.rs`: Bob's channel balances do not move).
- Not done: CLI `allod accounts …` (§14; no CLI client commands exist yet).

**BIP-353 sending (2026-11-07).** `wallet/bip353.rs` resolves names as in §11.3 (plain DNS, then a `dnssec-prover` proof checked locally); tests: a captured real proof for `matt@mattcorallo.com` checked at its capture time (valid, other name empty, expired, tampered), fixed-record wallet tests (pays the offer, payer note, counterparty `bip353`, several records / on-chain only / missing `₿` name refused, balance untouched), and one live test behind `--ignored`.

**M8 public name implementation notes (2026-11-06; deSEC removed 2026-11-07).** §12.1 with exposures `private` and `public`; `proxy` (§12.2) is not built yet.
- NIP-05 acceptance: valid npubs on served own domains resolve through the public Host-scoped endpoint with CORS; unserved or foreign hosts do not, duplicate names are refused, and owner writes require a full session. Core and API tests cover these cases.
- `allod_core::public_name`: `PublicNameService` (state in `config.public_name`). A background task reconciles every 5 minutes and soon after a change (accounts created or closed, settings), at most every 20 s: BIP-353 records, twelve.cash and clank.money names, reachability.
- Routes: `GET/PUT /api/public-name`, `POST /api/public-name/refresh`, the twelve.cash and clank.money routes (§12.3, §12.4), and `GET /api/internal/tls-ask?domain=` (no auth) for Caddy.
- BIP-353 offers: each account's record uses the long-lived variable-amount offer made when the account was created, or else any active offer of the account without amount, expiry or single use and not made for a registry name; accounts created before a name existed get one on the next reconcile.
- Own domain: Allod lists the TXT records (and, with `public`, asks for an A record) but does not check them by DNS yet.
- Caddy: the `https` profile serves `{$ALLOD_DOMAIN}` (default `localhost`) plus a catch-all `https://` site with on-demand TLS; both answer `/api/internal/*` with 404. `ACME_EMAIL` is optional: without it the caddy service drops the `email` line before starting (Let's Encrypt accounts need no contact address). Without a domain, `install.sh` points to twelve.cash and clank.money names in Settings → Lightning addresses.
- Not built yet: the setup wizard step, the gossip-announcement warning, and LUD-16/LNURL serving itself (so with `public`, wallets that need LNURL still fail until LUD-16 receiving lands).
- Tests: `allod-core` `public_name::tests` (own domain, `public` certificates, an old `desec` setting reads as `none`, BIP-353 records following accounts, twelve.cash, clank.money, name checks), `allod-api/tests/public_name_flow.rs` (own domain records, `tls-ask`, input and auth, mainnet-only registries).

**Brought forward from M8 (2026-10-28): Lightning addresses, LNURL, Nostr contacts, counterparties.**
- Contact outbox lookup continues when index relays find a Lightning address but no BOLT12 offer; the contact's own relays may hold a newer kind 0 profile or offer list.
- `allod-lnurl`: LNURL-pay client for Lightning addresses (LUD-16), `lnurl1…` (LUD-06) and `lnurlp://` (LUD-17), with comments (LUD-12). https only; plain http is allowed for localhost on regtest. Before paying, the invoice must be for the requested amount, and its description hash (when present) must be the SHA-256 of the metadata.
- `POST /api/payments` and `/api/payments/decode` accept them (`kind: "lnurl"` with `min_msat`, `max_msat`, `comment_allowed`; `payer_note` becomes the comment). The payment's metadata records `recipient_data.identifier` and `comment` (NWC-06), and `recipient: {pubkey, name}` when Send pays a contact. NWC `pay` (NWC-321) still takes BIP-321 only (`UNSUPPORTED_PAYMENT_INSTRUCTION` for addresses). NWC `pay_invoice` now takes BOLT11 only.
- Nostr contacts (`allod_nwc::contacts`): the owner's newest kind 3 follow list, and for each followed account its newest kind 0 profile, kind 10058 offer list (NIP-B1 draft, nostr-protocol/nips#2421: `["offer", "lno1…"]`) and kind 10133 payment targets (NIP-A3 `payto`: `lightning` addresses, and `lno=` inside a `bip321` URI, proposed in #2522). Cached for 10 minutes. Allod prefetches every owner's contacts in the background 5 s after start and refreshes them every 9 minutes (before the cache expires), so contact search in Send answers from memory; an owner added later is picked up on the next round. Each contact gets `targets` ordered BOLT12 offers first (faster and more private), then Lightning addresses, then LNURL; the legacy kind 0 keys `lno`, `bolt12` and `offer` also count as offers. Contact queries use `ALLOD_PROFILE_RELAYS` (default purplepag.es, nos.lol, relay.primal.net), separate from NWC `ALLOD_RELAYS`; regtest sets profile relays to its local relays. `GET /api/contacts?refresh=` and `GET /api/profiles?pubkeys=` (≤100). Outbox model (NIP-65): the owner's kind 3 is also fetched from the owner's write relays (kind 10002). Follows still without a BOLT12 offer after the index relays are looked up on their own write relays: up to 2 relays each, at most 25 relays per lookup, through a temporary client, skipping relays already asked and private/non-`wss` relays. On a 764-follow list this found a few more payable contacts (598 → 599 payable, 664 → 665 named, ~15 s instead of ~6 s): most unresolved follows publish neither a profile nor a relay list. Index requests are batched (50 accounts × 3 kinds, 4 in parallel, then 15-account retries) because relays cap events per request. The header shows the owner's Nostr name and picture. In Send, the payment field doubles as contact search (name, username, NIP-05 or address; arrow keys and Enter). Picking a contact selects its first target, and a contact with several targets gets a choice.
- Counterparty (`wallet::counterparty`): every transaction carries `counterparty {pubkey?, name?, identifier?, comment?, source}`, read from a NIP-57 zap request (in metadata `nostr` or as the invoice description), LUD-18 `payer_data`, a LUD-12 `comment`, a BOLT12 payer note, or `recipient` / `recipient_data` for outgoing payments. This is what the payer claims; nothing is verified. Activity shows "From X" / "To X" with the profile name and picture.
- CSP `img-src` allows `https:` for profile pictures (no referrer is sent). The image host sees the viewer's IP address.
- Build output: `[profile.dev]` uses line-table debug info only, and dependencies have none. `just trim` (also run by `just check` and `just e2e`) drops build output not read for 2 days and old incremental sessions. A full workspace build plus tests went from about 60 GB of accumulated output to 2.5 GB.

**Fast sync (AssumeUTXO), optional, chosen in setup (2026-10-28).**
- The setup wizard has an "Initial sync" step after the backup check. Full validation is the default. "Fast sync from a snapshot" takes a URL, prefilled for mainnet (935,000), signet (290,000) and testnet4 (120,000) from bitcoin-snapshots.jaonoctus.dev and editable. The step only appears when fast sync is available and Bitcoin is still in IBD, and it skips itself if the check fails. Fast sync can also be requested later through the API while still in IBD.
- Mechanism: Allod and the bundled bitcoind share a `bitcoind-control` volume (`/control`, owned by uid 1000 in both images). `POST /api/sync/fast {url}` (full permission; refused once bitcoind is synced) writes `fastsync-request.json`. `images/bitcoind/assumeutxo.sh`, started by the entrypoint when `/control` is writable, does the rest:
  1. downloads to `/data/utxo-snapshot.dat`. Network drops are survived: curl gives up below 10 KB/s for 2 minutes, a watchdog kills it after 3 minutes without growth, and every retry resumes where the file stopped (`-C -`), with backoff, until it completes or is canceled. While downloading, bitcoind only syncs headers: once it knows the snapshot's base header, the helper pauses networking (`setnetworkactive false`, re-applied after a restart) so no blocks from genesis are downloaded meanwhile, and switches it back on after loading, failure or cancel. Bitcoin Core has no startup option for a snapshot (`loadtxoutset` is RPC-only and needs the base header), so bitcoind has to run;
  2. reads the base block hash from the snapshot header;
  3. waits until bitcoind has that header;
  4. runs `loadtxoutset`, then deletes the file;
  5. reports `downloading | download_error | waiting_for_headers | loading | loaded | failed | not_needed | canceled` in `fastsync-status.json`.
  `GET /api/sync/fast` returns availability, the request, the status and the suggestion; `DELETE` cancels before loading. The wizard and Home show a "Fast sync" row.
- Offered only with `ALLOD_CONTROL_DIR` (set in the image) while the node reads the chain from the stack's own bitcoind: `ALLOD_CHAIN=bundled|bundled-rest`, or the standby bitcoind once the owner switched to it (§2.3.2; the standby container runs the same helper). Otherwise `GET` reports `available: false` and `POST` answers `501`. After setup, Home's "Getting ready" card offers it too ("Use fast sync", with the same explanation and prefilled URL) while bitcoind is in IBD and no snapshot was requested, so an owner who skipped it in the wizard, or who just started the standby node, can still take it. `ALLOD_FASTSYNC_URL` (http(s) URL as the bundled bitcoind reaches it) replaces the mirror's suggestion with a snapshot shipped with the device (source "this device"; height from the file name `utxo-[<network>-]<height>.dat`, else the mirror's height); the appliance snapshot images set it (§2.3.10). bitcoind rejects any snapshot whose hash is not built into its release, so the mirror does not need to be trusted. The download (about 9–10 GB) needs that much free space temporarily.
- Progress in the UI: the "Getting ready" list shows a bar per step. Fast sync covers download, header wait and snapshot loading; the coin-loading percentage comes from bitcoind's `[snapshot] N coins loaded (P%)` log lines, which the helper reads while `loadtxoutset` runs. Bitcoin is counted from the snapshot's block once loaded, and bitcoind's own progress is hidden until then. Each bar has a time-left estimate from the measured rate: bytes or percent over the last 2 minutes for the download and loading; blocks per second (after a snapshot) or `verificationprogress` (full validation) over the last 10 minutes.
- An interrupted `loadtxoutset` (shutdown, restart, lost RPC connection) keeps the snapshot file and retries: up to 3 times, then `failed` with the file kept. Only errors about the snapshot itself (unknown height, hash mismatch, unreadable) delete the file.
- Signet finding, still valid: while background validation runs, a pruned node keeps only about the last 1,050 blocks of the snapshot chain, so ldk-server must not stay offline longer than that during that phase.

**Initial sync tuning (2026-10-28).** While bitcoind is in its first IBD, `images/bitcoind/sync-tuning.sh` gives `bitcoin.conf` a sync profile sized to the machine. `dbcache` = 60% of the memory above 2 GiB (the container's cgroup limit counts if lower), between 450 MiB and 16 GiB and never below `BITCOIN_DBCACHE`. `blocksonly=1`. `par` is set only when a cgroup CPU quota is below the visible cores. A watcher marks `/data/.initial-sync-done` once `initialblockdownload` is false, then stops bitcoind; the restart policy brings it back with the normal profile (`BITCOIN_DBCACHE`, transaction relay on, which ldk-server's fee estimation needs). Both settings are read only at startup, so one restart is unavoidable. With fast sync, the profile lasts until the snapshot chain reaches the tip and also speeds up loading. Background validation of old blocks continues with the normal profile. **Block filter index:** the sync profile runs with `blockfilterindex=0`, since building it costs CPU and blocks it has not indexed cannot be pruned (with fast sync, every block after the snapshot would stay on disk until background validation finishes). After the sync an unpruned node builds it from its stored blocks. A pruned node does not: a new index needs every block since genesis, and bitcoind refuses to start one once blocks are pruned ("best block of the index goes beyond pruned data"); the entrypoint logs a warning instead. An index that already exists (`indexes/blockfilter/basic`) is never switched off by the profile, as it would fall behind pruned data. `blockfilterindex=` in `BITCOIN_EXTRA_CONF` wins. `BITCOIN_SYNC_TUNING=off` disables it; `dbcache=`, `blocksonly=` and `par=` in `BITCOIN_EXTRA_CONF` are never overridden; regtest is excluded. Tested by `images/bitcoind/test-sync-tuning.sh` (`just test-images`); verified live on mainnet: 31.8 GB and 22 CPUs gave `dbcache=16384`, `blocksonly=1`.

---

## 18. Open items (resolve before or during the named milestone)

| ID | Item | Milestone |
|---|---|---|
| O-1 | ldk-server: expose claimable state + `claim_deadline` via `GetPaymentDetails`, so a hold invoice accepted while Allod was disconnected can be recovered. Propose upstream; until then use the fallback in §7.6.3. | M3 |
| O-2 | ldk-server: `issuer` field on `Bolt12Receive`. | M5 |
| O-3 | ldk-server: manual invoice-request handling (or an "offer disabled" flag) to support `single_use` offers and offer disabling. Until then, "disable offer" in the UI only sets the offer to `disabled`, and payments that still arrive are still credited to the owner. The UI explains that a BOLT12 offer cannot be revoked once shared. | M5 |
| O-4 | ldk-server: `min_cltv_expiry_delta` on `Bolt11ReceiveForHash`. | M5 |
| O-10 | Switching ldk-server's chain source on an existing node (bitcoind ↔ Esplora/Electrum) is offered in Settings (§2.3.2) by owner decision. Still to confirm against ldk-node (PR #963, chain-source switching tests): wallet and channel-monitor sync after each switch, and the catch-up path when returning to a pruned bitcoind. | M6.5 |
| O-11 | `ldk-server-client` pins reqwest 0.11 / hyper 0.14 / rustls 0.21, which carry RUSTSEC advisories (listed as exceptions in `deny.toml`). Upstream PR to move the client to hyper 1 / rustls 0.23, then drop the exceptions. | M2 |
| O-12 | ldk-node retries bitcoind RPC calls (fee estimation) without backoff. While bitcoind is in initial block download this exhausts local ports (`os error 99`) and stalls sync. Workaround in M0: `ldk-entrypoint.sh` starts ldk-server only after IBD, plus TCP sysctls. Report upstream with a reproduction. | M2 |
| O-13 | ldk-server: return the preimage from `Bolt11Receive` (ldk-node can derive it), or store pending inbound payments so `GetPaymentDetails` finds them. Today a fresh invoice has no payment record, so a self payment of a normal invoice settles internally without a preimage. | M5 |
| O-8 | ldk-server: encrypted `keys_mnemonic` (encrypted with a key Allod provides) so the managed-mode seed is not plaintext on disk. | M6.5 |
| O-9 | LDK peer storage (channel state stored encrypted with peers) exposed by ldk-node/ldk-server, as an extra backup layer. | M10 |
| O-5 | Confirm that the ldk-node bolt11 `payment_id == payment_hash` invariant holds for the pinned version (e2e assertion). | M3 |
| O-16 | twelve.cash: ask for an authenticated way to update or delete a name (their roadmap lists editing), so a closed account's address can be removed and an offer change does not need a new name. | M8 |
| O-15 | BIP-353 wallet support: list wallets that resolve BIP-353 and pay BOLT12 (shown with exposure `private`), and recheck before release. Decide whether the Allod project runs a public `lnurl-proxy`, and under which name. | M8 |

---

## 19. Glossary
- **App / connection**: an NWC client identity (`app_pubkey`) with permissions.
- **Account**: an isolated app created for a person or device, with its own ledger account (Alby Hub calls this a sub-wallet). **Main** is the built-in account for unallocated funds.
- **Main wallet**: node funds not allocated to isolated apps.
- **Wallet key**: the per-app Nostr key Allod uses to sign responses and notifications for that app.
- **Self payment**: a payment whose target was created by this Allod instance. It is settled in the DB without touching the Lightning network.
