# Allod — orientation for agents

Allod is a self-custodial Lightning wallet service written in Rust. It runs next to an
[ldk-server](https://github.com/lightningdevkit/ldk-server) node and gives its owner a web UI,
a REST API, and Nostr Wallet Connect. License: MIT.

## What it does

- **Node**: manages the ldk-server node it deploys with (start/stop, channels, peers, on-chain
  funds, message signing, node graph), and reads sync progress from bitcoind. The Node page's
  Bitcoin node card (`GET /api/node/bitcoind`, one batched RPC) shows Bitcoin Core's version, tip,
  peers, mempool, disk, warnings, fee estimates and, after a fast sync, background validation.
- **Wallet**: pays and receives BOLT11 and BOLT12 (offers, `pay` to an offer), hold invoices,
  on-chain sends/receives, and transfers between accounts. Send also resolves Lightning
  addresses, LNURL-pay, and Nostr contacts (BOLT12 offer, NIP-B1/kind 10133/profile payment
  targets). Activity records the payer (zaps, payer data, comments).
- **Prisms**: an own Lightning address can forward a share of every payment it receives to
  other Lightning targets (LNURL, a Lightning address, a BIP-353 name or a BOLT12 offer).
  Percentages need not add up to 100%: the rest stays in the address's wallet. Each payout's
  state is kept (migration `0020`), so a split that could not be sent is retried with backoff
  and the amount that never arrived stays visible. Failed payouts stay separate, each with its
  own destination and amount; they are never merged into one payment. While any remain, the
  address row, Settings navigation and main menu show a warning. A split can carry a note for
  the owner — what or who the destination is — which is never sent and is kept when the splits
  are saved again. A split keeps its identity when edited, so pointing it at a new destination
  moves what it never sent (pending or failed sats) there; paid history keeps its original
  destination. The owner can clear a split's failed total (it is marked canceled, never paid),
  and `GET /api/addresses/{id}/prism/payouts` lists every payout newest first, one page at a
  time, for the history shown in the Prism tab. A split may not pay one of
  Allod's own addresses that forwards back to the prism (checked when saving and again before
  every payout), so a prism cannot loop money through itself. A destination is checked when
  the splits are saved (BIP-353 record, LNURL endpoint, offer syntax); one that answers
  nothing is refused rather than failing on every payment.
- **Nostr Wallet Connect (NIP-47)**: full method set — `get_info`, `get_balance`, `get_budget`,
  `pay_invoice`, `multi_pay_invoice`, `pay`, `make_invoice`, `make_hold_invoice`,
  `settle_hold_invoice`, `cancel_hold_invoice`, `receive`, `make_offer`, `lookup_invoice`,
  `lookup_payment`, `list_transactions`, `sign_message`, `create_connection`,
  `register_push`/`unregister_push` — plus NWC extensions 02, 03, 05, 06, 08, 09, 12 and 321.
  Keysend (`pay_keysend`, NWC-04) is deliberately unsupported.
  NWC-08 (an app brings its own key): the confirmation page is `/connections/new?pubkey=…`,
  also reached by pasting a `nostr+walletauth://` link on New connection. Such a connection
  gets exactly the methods and notification types it asked for (`apps.methods`,
  `apps.notification_types`, migration `0023`), not everything its permissions cover, until
  the owner edits its permissions. Its info event names the app (`p`), repeats its `state`
  and lists Allod's relays, and goes once to the relays of the request.
- **Lightning connectivity**: Settings → Lightning → Connectivity decides who can open
  connections to the node: Tor on/off, plus one internet path — none, a gateway's IPv4
  (home IP hidden) or the home IP (router port forward). Both off is private (ldk-server
  listens on loopback, announces nothing). Stored as `lightning.connectivity` in `config`;
  the first pass saves a starting choice (the older `lightning.gateway` as gateway mode,
  else `ALLOD_ANNOUNCE_ADDRESSES` as home IP plus Tor for an onion, else private), so the
  UI always shows the option in use. The `tunnel` and `tor-proxy` services always run, so
  every option works on Compose without profiles. Allod writes
  `{handoff}/lightning-announce.json` (`addresses` replace `LDK_ANNOUNCE`; `listen`:
  `loopback`, `internal` = 9735 inside the stack only, `all` = also `LDK_HOST_LISTEN`, the
  container port behind the published host port), `{tunnel_dir}/lightning` +
  `routes/<tunnel id>.lightning` (gateways poll it every minute), and `{tor_dir}/lightning`
  (tor-proxy adds the onion service on SIGHUP and writes `lightning.onion` back over the
  shared `tor-control` volume). Allod probes each path like a peer (onion through
  `ALLOD_TOR_PROXY`) and shows the result on Node → Overview. Outbound connections to
  clearnet peers still leave from the home IP.
- **Accounts**: isolated sub-wallets (Alby Hub style) on one double-entry ledger, each with its
  own statement, budget-free top-up/withdraw, and its own NWC connections, NIP-05 names and
  Lightning addresses.
- **Public names**: NIP-05, Lightning addresses and BIP-353 names on domains the owner brings
  (with DNSSEC; DNS managed at deSEC or Njalla, domains bought through Njalla, an LNVPS gateway
  rented when the node has no public address), and third-party names on twelve.cash,
  clank.money, ZEUS Pay and Alby.
- **Access**: owners sign in with Nostr — NIP-07 browser extension or NIP-46 remote signer —
  and the API accepts NIP-98-authorized requests. Push notifications go to browsers/PWA.
- **Deployment**: one Docker Compose bundle for VPS, wrapped as an umbrelOS app, a StartOS
  package, and a bootable ISO / Raspberry Pi image (proof of concept).

## Ground rules (highest priority)

- **Money**: never mark a payment settled or failed without the backend confirming it; validate
  balance and NWC budget under the ledger lock; every balance change is a balanced
  double-entry posting.
- **Access**: scope every API/NWC read and write by the owning connection or account; a
  connection never sees another app's payments.
- **Secrets**: recovery phrase, node keys and third-party tokens live as files in
  `{data_dir}/secrets/`, never in the database and never in logs.
- **No unlock step**: Allod trusts the machine it runs on — there is no password. Anything that
  weakens the host boundary must stay explicit (full-disk encryption is the owner's job).
- Public names must be re-checked against DNS before the UI calls them live; a purchase is
  never finished on the strength of a local guess.

## Layout

| Path | What |
|---|---|
| `crates/allod` | binary: `serve` daemon, CLI (`setup`, `setup-link`, `owner`, `healthcheck`), config loading |
| `crates/allod-api` | Axum HTTP API, auth (NIP-98), setup/claim routes, embedded web UI |
| `crates/allod-core` | business rules: setup, wallet + ledger, accounts, apps/NWC budgets, public names, domains, push |
| `crates/allod-db` | SQLite/PostgreSQL access; `migrations/{sqlite,postgres}` must stay in step |
| `crates/allod-ln` | `LightningBackend` trait, ldk-server client + event hub, mock backend for tests |
| `crates/allod-nwc` | relay subscriptions, request pipeline, NIP-47 handlers, contacts/zaps |
| `crates/allod-lnurl` | LNURL-pay client and LNURL server helpers |
| `crates/allod-keys` | BIP39 phrase, Nostr keys, file-backed secret store |
| `crates/allod-types` | shared enums and API types (network, chain mode, deployment info) |
| `crates/allod-gossip` | gossip sidecar (RGS snapshots verified against bitcoind's UTXO set) |
| `crates/allod-e2e`, `allod-devtools` | e2e harness and developer CLI (`nwc-uri`, `nwc`, `check`) |
| `web/` | React 19 + Vite + Tailwind UI (embedded into the binary); Vitest + Playwright |
| `images/`, `deploy/` | Dockerfiles/entrypoints and the production Compose bundle + regtest overlay |
| `allod/`, `umbrel-app-store.yml`, `packaging/startos`, `iso/` | platform packages and appliance images |

## First run and sign-in

- Interactive: `allod setup-link` prints a one-time browser link (`#setup=<token>`); the wizard
  connects a signer (becomes the owner), then creates or restores the recovery phrase and hands
  it to ldk-server. `--rotate` invalidates earlier links.
- Headless: `allod setup --owner <npub|hex> [--restore-file phrase.txt --birthday-height N]`.
- Owner keys are managed on the host: `allod owner add|list|remove`. Removing the last owner is
  refused; this is also the recovery path when a key is lost.
- After sign-in the browser holds a session token; the web UI never sees the recovery phrase
  again except on the Backup screen.

## Run it

### 1. Installer (VPS, Debian/Ubuntu)

```sh
curl -fsSL https://allod.me/install.sh | sh
```

The installer finds the release on Nostr (signed by the Allod release key) and downloads
it from Blossom with `allod release fetch` (`crates/allod-core/src/release.rs`), run from an
allod image that `scripts/render-bundle.sh` pins by digest in the released `install.sh`.

Installs to `/opt/allod` as root, or `~/allod` for a user with Docker access; installs Docker
only if missing. `--nightly` installs the nightly build (signet/testnet4 test only).
New installs run on mainnet unless `--network signet|testnet4` is given (not asked); no node
alias is set unless `ALLOD_ALIAS` is (public channels need one).
Without `--dir` it updates the stack of Compose project `allod` it finds (asking first).
`--name NAME` sets `COMPOSE_PROJECT_NAME` for a second stack on one host (own `--dir` and
ports too); a name already used by a stack in another directory is refused.

### 2. Docker Compose by hand

```sh
cd deploy
cp .env.example .env      # network, chain mode, passwords, COMPOSE_PROFILES
docker compose up -d
docker compose exec allod allod setup-link --base-url http://127.0.0.1:8029
```

The UI listens on `127.0.0.1:8029`. Reach it through an SSH tunnel, the `tor` profile, or the
`https` profile (Caddy + Let's Encrypt for `ALLOD_DOMAIN`).

### 3. umbrelOS

Add `https://github.com/frnandu/allod` as a community app store, then install Allod. The app
uses Umbrel's Bitcoin Node when installed (its own pruned node otherwise) and opens the wizard
through `app_proxy` with no setup link. See `docs/umbrel.md`.

### 4. StartOS

`packaging/startos` builds a signed `.s9pk`; the repo also acts as its registry index.
See `packaging/README.md`.

### 5. Appliance ISO / Raspberry Pi (proof of concept)

`just iso`, `just iso-rpi`, `just iso-run` — see `iso/README.md`.

### 6. Regtest stack (development)

Requires `just`, Docker Compose v2, Rust 1.95, Node 22 with corepack.

```sh
just up                # build + start bitcoind, ldk-server (Alice), Bob/Carol, Electrs,
                       # relays, gossip sidecar, Allod; auto-setup and channel funding
just up-manual         # same, without auto-setup: run `just allod setup --owner npub1…`
just doctor            # verify every service and the channel ring
just bob bolt11-receive 10000sat
URI=$(just nwc-uri); just nwc "$URI" get_balance
just down              # stop, keep data        just reset   # stop + delete regtest data
```

The regtest stack uses image tag `:regtest` and its own ports/volumes, so it can run next to a
real deployment on one machine.

### 7. Split host development

```sh
just run               # Allod on the host against the regtest stack (UI 127.0.0.1:18029)
just infra             # only the Docker side, if Allod already runs elsewhere
just web-dev           # Vite with hot reload, proxying to ALLOD_API_URL
just host-dev          # host Allod + Vite against the production stack in deploy/compose.yaml
```

`just ship <ssh-target> [services] [dir]` (`scripts/ship.sh`) builds stack images here and
swaps them into a running stack elsewhere over SSH, for tests without a release. It writes
nothing on that host besides the images: its next `docker compose up -d` or update restores
the stack's own images. `ALLOD_SHIP_CHECK=1` only checks the remote stack.

`just run` starts `just infra-fast` when needed and finishes bootstrap (Alice's phrase comes
from the host process, her TLS cert and macaroon come back through `deploy/.dev`).

> **host-dev caveat**: `scripts/host-dev.sh` runs a cleanup trap on exit that stops the host
> processes, restores the Allod container and chowns the named volumes back to the container
> uid. Killing one of its children (the Allod process, Vite) runs that cleanup: the host
> backend then loses write access to its SQLite file (`attempt to write a readonly database`)
> and Vite/proxy disappear. Stop the script itself, or restore ownership with its own
> `chown -R 0:1000` volume command before restarting pieces by hand.

### 8. Tests

```sh
just check             # fmt-check, clippy, Rust + web unit tests, web lint, image tests, trim
cargo test --workspace # Rust only
(cd web && corepack pnpm test)
just e2e               # doctor + API/NWC scenarios against a running regtest stack
just e2e-ui            # fresh stack; Playwright drives the setup wizard and sign-in
just e2e-matrix        # the stack against every chain mode
```

## Configure

Precedence: **CLI flags > environment > TOML file (`ALLOD_CONFIG`) > defaults**. `allod serve`
takes every setting as a flag and env var; `deploy/.env.example` documents the stack-level
ones. Defaults: data dir `~/.allod`, database `sqlite://~/.allod/allod.db`, listen
`127.0.0.1:8029`, secrets in `~/.allod/secrets`.

Settings that matter most in a deployment:

| Setting | Meaning |
|---|---|
| `ALLOD_NETWORK` | `bitcoin` (default), `signet`, `testnet4`, `regtest`; startup fails if ldk-server disagrees |
| `ALLOD_CHAIN` | how the stack reaches the chain: `bundled`, `bundled-rest`, `external-bitcoind`, `electrum`, `esplora`; switchable at runtime in Settings → Bitcoin |
| `COMPOSE_PROFILES` | `bundled` (bitcoind in the stack) plus optional `https`, `tor`, `backup`, `postgres` |
| `ALLOD_HTTP_BIND` | UI bind address (`127.0.0.1:8029`); keep loopback and front it with HTTPS/Tor |
| `ALLOD_PUBLIC_URL`, `ALLOD_DOMAIN`, `ACME_EMAIL` | public HTTPS URL used in links and the `https` profile's certificate |
| `ALLOD_RELAYS`, `ALLOD_CLIENT_RELAYS`, `ALLOD_PROFILE_RELAYS` | NWC relays, relays written into NWC links, relays for contacts/profiles |
| `ALLOD_NODE_EXPLORER_URL` | explorer link template (`{pubkey}`); empty disables links |
| `ALLOD_DNS_RESOLVER` | DNS server (`ip:port`) for the DNSSEC proofs used with BIP-353 names |
| `ALLOD_DATABASE_URL` | `sqlite://…` (default) or `postgres://…` (with the `postgres` profile) |
| `ALLOD_LOG`, `ALLOD_LOG_FORMAT` | tracing filter (default `info`) and `text`/`json` |
| `BITCOIN_PRUNE`, `BITCOIN_DBCACHE`, `BITCOIN_MEMORY_LIMIT`, `BITCOIN_SYNC_TUNING` | bundled bitcoind sizing (`BITCOIN_PRUNE=auto` sizes to the disk) |
| `LDK_LSPS2_LSP`, `LDK_LSPS2_LSP_TOKEN` | LSP for just-in-time receive channels; empty disables |
| `LITESTREAM_*` | `backup` profile replication of the SQLite databases |

- `ALLOD_MODE=managed` (bundle with ldk-server) or `standalone` (an ldk-server you run
  yourself, with `ALLOD_LDK_SERVER_ADDRESS`/`CERT_FILE`/`MACAROON_FILE`).
- Secrets never enter the database: third-party tokens, the recovery phrase and the node
  credentials are files under `{data_dir}/secrets/`.
- `ALLOD_SKIP_BACKUP_CHECK` and the hidden `ALLOD_*_URL` stand-ins are testing-only.

## Working in the code

- Keep HTTP errors in the `{"error": {"code", "message"}}` shape; use the `ApiError` helpers.
  Extra machine-readable fields (e.g. `invoice`) go next to `message`.
- Migrations: add one to `crates/allod-db/migrations/sqlite` **and** `.../postgres`, same
  version number, and keep both dialects working (CI runs SQLite; PostgreSQL is supported).
- Money paths belong in `allod-core` (`wallet`, `apps`, `accounts`), tested under
  `#[cfg(test)] mod tests` next to the code; handlers stay thin.
- New config is a field on `ConfigArgs` with an env var, plus `.env.example` and README/AGENTS
  updates when it affects a deployment.
- Web: SWR for server state, Tailwind tokens (`bg-surface`, `text-muted`, `border-line`…), UI
  primitives from `components/ui`, one screen per route; add a Vitest case in
  `web/src/App.test.tsx` for user-visible flows.
- Tests run without Docker (`cargo test --workspace` skips `#[ignore]`d e2e); anything that
  needs the stack belongs in `allod-e2e` or Playwright.

## Checks before you push

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd web && corepack pnpm lint test build   # pnpm from web/: corepack needs web/package.json's pin
just compose-check               # when touching deploy/compose.yaml or .env.example
```

This repository's forge is Grasp (git over Nostr; ngit, gitworkshop.dev),
not GitHub or GitLab. Use ngit for issues, pull requests, reviews, CI and
releases. Before any of those tasks, including pushing a pull request
branch, run `ngit --help` and follow its workflow map. If ngit is
missing, install it from https://ngit.dev/install.
