# Allod

> *Allodium*: land held in absolute ownership, owing no lord.

Allod is a self-custodial Lightning node you deploy yourself: Bitcoin Core (or a light
backend), [ldk-server](https://github.com/lightningdevkit/ldk-server), and the Allod wallet
service with Nostr Wallet Connect, accounts and a web UI. It runs on a VPS with Docker
Compose, and is being packaged for Umbrel and StartOS.

**Status: early development (milestone M5).** The deployable stack, first-run setup, Nostr
sign-in, node management (channels, peers, on-chain), the wallet (BOLT11, BOLT12, hold
invoices, double-entry ledger) and Nostr Wallet Connect with budgets, notifications and the
extensions 02, 03, 05, 06, 08, 09, 12 and 321 work. Send pays Lightning addresses, LNURL and
your Nostr contacts (BOLT12 offer or Lightning address from their profile), and Activity
shows who a payment came from (zaps, payer data, notes). 

## Run the stack

On a Debian or Ubuntu VPS, the installer sets up the stack (it asks for the
network, chain source, domain, Tor and backups, and installs Docker if needed), then prints
how to start it:

```sh
curl -fsSL https://github.com/allod-me/allod/releases/latest/download/install.sh | sh
```

The script asks its questions on the terminal, so it works through a pipe. Options go after
`sh -s --`, e.g. `| sh -s -- --version 0.1.1 --dir ~/allod`; `--nightly` installs the latest
nightly build instead (untested, for signet/testnet4; not for real funds):

```sh
curl -fsSL https://github.com/allod-me/allod/releases/download/nightly/install.sh | sh -s -- --nightly
```

To read and verify the script before running it:

```sh
curl -fsSLO https://github.com/allod-me/allod/releases/latest/download/install.sh
curl -fsSLO https://github.com/allod-me/allod/releases/latest/download/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS && less install.sh
sh install.sh
```

As root it installs to `/opt/allod`; as any other user who can run Docker (docker group or
rootless Docker), to `~/allod`. It only needs sudo to install Docker when it is missing.

Or by hand, from a checkout:

```sh
cd deploy
cp .env.example .env    # set ALLOD_NETWORK, ALLOD_CHAIN, passwords, COMPOSE_PROFILES
docker compose up -d
docker compose exec allod allod setup-link --base-url http://127.0.0.1:8029
```

The UI listens on `127.0.0.1:8029`. Use an SSH tunnel, the `tor` profile, or the `https`
profile (Caddy with Let's Encrypt for `ALLOD_DOMAIN`) for remote access.

Open the one-time link printed by `allod setup-link`, then follow setup wizard: connect a Nostr signer (browser extension or a
NIP-46 signer app such as Amber or Peridot), which becomes the owner key, then create or
restore the recovery phrase. Allod hands the phrase to ldk-server, which starts once Bitcoin
has synced. There is no unlock step: Allod trusts the machine it runs on, so use
full-disk encryption on hardware you own. A lost owner key is replaced on the host with
`docker compose exec allod allod owner add <npub>`.

Chain modes (`ALLOD_CHAIN`): `bundled` (pruned Bitcoin Core in the stack, default),
`bundled-rest`, `external-bitcoind`, `electrum`, `esplora`.

On umbrelOS, add this repo (`https://github.com/allod-me/allod`) as a community app store
(App Store, `...`, Community App Stores) and install Allod from it. See
[docs/umbrel.md](docs/umbrel.md).

## Lightning addresses and prisms

An address on a domain you bring (`you@yourdomain`, BIP-353 and/or LNURL) can be a
[prism](https://dergigi.com/2023/03/12/lightning-prisms/): each payment it receives is divided
by percentages you set, and each split is forwarded to another Lightning target — an LNURL or
Lightning address, a BIP-353 name, or a BOLT12 offer. Percentages need not add up to 100%: the
remainder stays in the address's wallet, and splits under 1 sat are skipped. Every payout keeps
its own state, so a destination that cannot be reached is retried with a growing delay, and the
amount that never arrived stays visible (with a "Retry now" in Settings → Lightning addresses →
address → Prism) instead of disappearing into a failed payment. Failed payouts are kept one by
one, each with its own destination and amount rather than merged into a single payment, and
the address list, Settings navigation and main menu warn while any remain. Each split also
takes a note for yourself (what or who the destination is); it stays on this machine and is
never sent with the payment. Editing a split's destination keeps its identity: what it never
sent follows it there, while what was already paid keeps its original destination. A split's
failed total can be cleared without sending anything, and the Prism tab opens a payout
history with each row's state, amount, fee, time and last error. Split destinations
are checked when you save them: a plain name must answer BIP-353 or LNURL, and an address that
answers nothing is refused. A split that would pay one of
your other Allod addresses which forwards back to this one is refused when saved, and the
worker checks again before every payout, so a prism cannot loop money through itself.

## Develop

Requirements: Rust 1.95, Node 22 with corepack (it provides the `pnpm` pinned in
`web/package.json`), Docker with Compose v2, `just`.

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

For that split, `just run` is enough: it starts `just infra-fast` (same stack, existing images, no
Docker build) when `deploy/.dev/alice.env` is missing, then finishes the stack in the background.
`just infra-fast` on its own returns as soon as `alice.env` exists, because bootstrap cannot
complete without Allod: host Allod hands Alice her recovery phrase through the shared
`deploy/.dev/handoff` directory, and Alice publishes her TLS cert and macaroon back into
`deploy/.dev/alice/`.

## Layout

| Path | What |
|---|---|
| `crates/` | Rust workspace (`allod` binary, API, DB, Lightning backend, NWC, devtools) |
| `web/` | React UI, embedded into the `allod` binary |
| `images/` | Dockerfiles and entrypoints (allod, ldk-server, bitcoind, tor) |
| `deploy/` | Production `compose.yaml`, regtest overlay, configs, bootstrap scripts |
| `allod/`, `umbrel-app-store.yml` | umbrelOS app package; this repo is its community app store |
| `docs/` | Platform guides (Umbrel) |
| `scripts/` | Release helpers |

## License

MIT
