# Allod

> *Allodium*: land held in absolute ownership, owing no lord.

Allod is a self-custodial Lightning node you deploy yourself: Bitcoin Core (or a light
backend), [ldk-server](https://github.com/lightningdevkit/ldk-server), and the Allod wallet
service with Nostr Wallet Connect, accounts and a web UI. It runs on a VPS with Docker
Compose, and is being packaged for Umbrel and StartOS.

**Status: early development (milestone M5).** The deployable stack, first-run setup, Nostr
sign-in, node management (channels, peers, on-chain), the wallet (BOLT11, BOLT12, hold
invoices, double-entry ledger) and Nostr Wallet Connect with budgets, notifications and the
extensions 02, 03, 05, 06, 09, 12 and 321 work. Send pays Lightning addresses, LNURL and
your Nostr contacts (BOLT12 offer or Lightning address from their profile), and Activity
shows who a payment came from (zaps, payer data, notes). 

## Run the stack

```sh
cd deploy
cp .env.example .env    # set ALLOD_NETWORK, ALLOD_CHAIN, passwords, COMPOSE_PROFILES
docker compose up -d
```

The UI listens on `127.0.0.1:8029`. Use an SSH tunnel, the `tor` profile, or the `https`
profile (Caddy with Let's Encrypt for `ALLOD_DOMAIN`) for remote access.

Open the UI and follow the setup wizard: connect a Nostr signer (browser extension or a
NIP-46 signer app such as Amber or nsec.app), which becomes the owner key, then create or
restore the recovery phrase. Allod hands the phrase to ldk-server, which starts once Bitcoin
has synced. There is no unlock step: Allod trusts the machine it runs on, so use
full-disk encryption on hardware you own. A lost owner key is replaced on the host with
`docker compose exec allod allod owner add <npub>`.

Chain modes (`ALLOD_CHAIN`): `bundled` (pruned Bitcoin Core in the stack, default),
`bundled-rest`, `external-bitcoind`, `electrum`, `esplora`.

## Develop

Requirements: Rust 1.95, Node 22 with corepack, Docker with Compose v2, `just`.

```sh
just check          # fmt, clippy, unit tests, web lint
just up             # regtest stack: bitcoind, Alice/Bob/Carol ldk-server, relays, Allod (set up automatically)
just e2e-ui         # fresh stack; Playwright runs the setup wizard and sign-in
just doctor         # verify every service and the channel ring
just e2e            # doctor, then the API and NWC e2e scenarios (crates/allod-e2e)
URI=$(just nwc-uri) # create a NWC connection; then: just nwc "$URI" get_balance
just bob bolt11-receive 10000sat    # ldk-server-cli against Bob
just e2e-matrix     # run the stack against every chain mode
just reset          # delete all regtest data
just trim           # drop build output unused for 2 days (also run by check / e2e)
```

`just infra` + `just run` runs Allod on the host against the regtest stack; `just web-dev`
starts Vite with hot reload. The regtest stack uses its own image tag (`:regtest`) and the UI on
`127.0.0.1:18029`, so it can run next to a real deployment on the same machine.

## Layout

| Path | What |
|---|---|
| `crates/` | Rust workspace (`allod` binary, API, DB, Lightning backend, NWC, devtools) |
| `web/` | React UI, embedded into the `allod` binary |
| `images/` | Dockerfiles and entrypoints (allod, ldk-server, bitcoind, tor) |
| `deploy/` | Production `compose.yaml`, regtest overlay, configs, bootstrap scripts |
| `scripts/` | Release helpers |

## License

MIT
