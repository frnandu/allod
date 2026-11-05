<p align="center">
  <img src="icon.svg" alt="Allod Logo" width="21%">
</p>

# Allod on StartOS

> Everything not listed in this document behaves as upstream Allod. See the
> Documentation section of `instructions.md` and the repository's `SPEC.md` (§2.3.6).

## Table of Contents

- [Image and Container Runtime](#image-and-container-runtime)
- [Volume and Data Layout](#volume-and-data-layout)
- [File Models](#file-models)
- [Dependencies](#dependencies)
- [Network Access and Interfaces](#network-access-and-interfaces)
- [Installation and First-Run Flow](#installation-and-first-run-flow)
- [Actions](#actions)
- [Tasks](#tasks)
- [Health Checks](#health-checks)
- [Backups and Restore](#backups-and-restore)
- [Limitations and Differences](#limitations-and-differences)
- [Quick Reference for AI Consumers](#quick-reference-for-ai-consumers)

## Image and Container Runtime

Four upstream images from the Allod release pipeline, one tag (`startos/images.ts`):

| Subcontainer | Image | Role |
|---|---|---|
| `allod` | `ghcr.io/frnandu/allod` | API, NWC service, web UI (port 8029) |
| `ldk-server` | `ghcr.io/frnandu/allod-ldk-server` | Lightning node (P2P 9735, gRPC 3536 on localhost only) |
| `gossip` | `ghcr.io/frnandu/allod-gossip` | Rapid Gossip Sync sidecar (port 8090, localhost only) |
| `bitcoind` | `ghcr.io/frnandu/allod-bitcoind` | Bundled pruned Bitcoin Core (RPC 8332, localhost only), or idle standby |

All run their image entrypoint as uid 1000. They share the package's network namespace
and reach each other on `localhost`. A `chown` oneshot fixes volume ownership on every start.

## Volume and Data Layout

One volume, `main`:

| Path | Mounted in | Contents |
|---|---|---|
| `allod/` | allod `/data` | `allod.db` (apps, accounts, ledger), secrets |
| `ldk/` | ldk-server `/data` | `ldk_node_data.sqlite` (channel state), TLS, macaroons |
| `handoff/` | allod and ldk-server `/handoff` | seed and credential handoff, chain-source requests |
| `gossip/` | gossip `/data` | network graph (re-downloadable) |
| `control/` | allod and bitcoind `/control` | standby start file (`bitcoind-enabled`) |
| `bitcoind/` | bitcoind `/data` | bundled node's blocks and chainstate (not backed up) |
| `startos-store.json` | package code only | `restorePending`, `chainSource`, bundled RPC passwords |

## File Models

`startos-store.json` (`startos/fileModels/store.json.ts`): `restorePending` (set on restore,
cleared by Confirm restore), `chainSource` (`startos-bitcoind` or `bundled`), and the bundled
node's RPC passwords, and `skipBackupCheck` (testing only).

## Dependencies

**Bitcoin Core** (`bitcoind`, optional; Core 28–31 or Knots). The chain source is picked
once, at the first start:

- Bitcoin Core installed: Allod uses it (RPC cookie read from its volume, read-only; a new
  cookie restarts Allod). The bundled bitcoind idles as a standby. If Bitcoin Core goes
  away, Allod shows a banner after 3 minutes offering to start the built-in node.
- Not installed: the bundled pruned bitcoind (`ghcr.io/frnandu/allod-bitcoind`) runs.

Installing or removing Bitcoin Core later does not switch the source; use Allod's
Settings → Bitcoin → Chain source.

## Network Access and Interfaces

| Interface | Port | Type |
|---|---|---|
| Web UI | 8029 | ui (LAN, Tor, clearnet as configured in StartOS) |
| Lightning P2P | 9735 | p2p |

## Installation and First-Run Flow

1. Optional: install and start Bitcoin Core first to use it instead of the bundled node.
2. Install and start Allod. Run **Open setup**, then click its LAN or Tor setup link.
   Allod removes the one-time secret from the address bar immediately. Sign in with a
   Nostr signer.
3. The setup wizard creates the node and its 24-word recovery phrase. ldk-server starts
   after Bitcoin Core finishes its initial block download.

## Actions

| Action | What it does |
|---|---|
| Open setup | Returns clickable LAN and Tor setup links before first-run setup |
| Show onion and connection info | Node ID, peer URIs (`pubkey@host:port`), Web UI addresses |
| Add owner key | Runs `allod owner add <npub>`: recovery path when a Nostr key is lost |
| Confirm restore | Hidden unless a restore is pending; clears the restore block |
| Allow skipping the recovery phrase check | Testing only: sets `ALLOD_SKIP_BACKUP_CHECK`, so the setup wizard shows Skip check (toggle) |

The recovery phrase is shown only inside the Allod UI after re-authentication, never
through an action.

## Tasks

After a restore, a critical **Confirm restore** task blocks startup until the user confirms
that the original node is permanently stopped (SPEC §2.3.7).

## Health Checks

| Check | Meaning |
|---|---|
| Network graph | gossip sidecar listening |
| Lightning node | ldk-server answers `get-node-info`; "loading" until setup and the Bitcoin sync finish |
| Web Interface | Allod listening on 8029 |
| Allod API | `/api/health` is `ok` (Allod and ldk-server answer) |
| Built-in Bitcoin node | idle standby, starting, or running |

With Bitcoin Core, its `sync-progress` check is listed as a dependency health check.

## Backups and Restore

StartOS stops the service and copies the `main` volume, minus `gossip/` and `bitcoind/`. A platform backup
is a last resort: restoring old channel state can lose channel funds. Use Allod's own
continuous backup (Settings) for channel state.

## Limitations and Differences

- Mainnet only (Bitcoin Core on StartOS is mainnet only).
- Outbound Tor for ldk-server is not configured.
- `x86_64` only until a multi-arch image tag is pinned.

## Quick Reference for AI Consumers

```yaml
package_id: allod
images: [allod, ldk-server, gossip, bitcoind]
volumes: { main: [allod, ldk, handoff, gossip, control, bitcoind] }
interfaces: { ui: 8029, peer: 9735 }
dependencies: { bitcoind: optional, chosen at first start, cookie auth via mount }
actions: [connection-info, add-owner, confirm-restore, skip-backup-check]
health_checks: [bitcoind, gossip, ldk-server, allod, api]
backup: volume main excluding gossip/ and bitcoind/
```
