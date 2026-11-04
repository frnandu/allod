# Allod on umbrelOS

This repo is also an umbrelOS community app store (SPEC §2.3.6): `umbrel-app-store.yml`
(store id `allod`) and the app package `allod/` sit at the repo root, because umbrelOS
only looks for `<app-id>/umbrel-app.yml` one level below the root.

## Install

1. In umbrelOS, open App Store, then the `...` menu, then Community App Stores.
2. Add `https://github.com/frnandu/allod` and open the "Allod" store.
3. Install Allod.

umbrelOS pulls the default branch (`master`) every 5 minutes, but only offers an update
when `version` in `allod/umbrel-app.yml` changes, so bump it with every package change.
Edits on `master` reach new installs right away.

## Chain source

The package declares no app dependency. On the first start, `exports.sh` checks for
Umbrel's Bitcoin Node app (`bitcoin`):

| Bitcoin Node app | Allod uses | Containers |
|---|---|---|
| installed | that node over RPC (`10.21.21.8:8332`, its credentials and network) | `allod`, `ldk-server`, `gossip`; `bitcoind` idles as a standby |
| not installed | its own pruned Bitcoin Core (sized to the disk, at most 50 GB), mainnet | the same four, `bitcoind` running |

`hooks/pre-start` saves the choice to `data/umbrel-chain.env`. Later starts keep it, so
installing or removing the Bitcoin Node app afterwards does not move the node. To change
the source, use Allod's Settings > Bitcoin > Chain source.

If the Bitcoin Node app stops answering for 3 minutes (for example you uninstalled it),
Allod shows a banner offering to start the standby node, which then syncs (hours to days)
before the Lightning node starts again. A public server in Settings is the faster
stopgap. To test it: install the Bitcoin Node app, install Allod, uninstall the Bitcoin
Node app, wait for the banner, click "Start built-in node", then check
`sudo docker logs allod_bitcoind_1` for `[standby] started by Allod`.

The Bitcoin Node app can itself be pruned: its Settings > Optimization > Prune Old
Blocks. Allod works with a pruned node.

## Test on a local umbrelOS VM

1. Serve the store from your working tree. The server binds to the libvirt bridge, so
   only VMs on it can reach it. Every fetch snapshots the working tree again, so you
   don't need to commit or push.

   ```sh
   just umbrel-store            # http://192.168.122.1:8765/allod-store.git
   ```

2. In umbrelOS, open App Store, then the `...` menu, then Community App Stores. Add
   `http://192.168.122.1:8765/allod-store.git`, then install Allod from the "Allod" store.

3. Iterate. Bump `version`, then refresh the store (or wait for the periodic pull). You
   can also uninstall and install again.

   To test the other chain mode, uninstall Allod, install or remove the Bitcoin Node
   app, and install Allod again. Uninstalling deletes the saved choice along with the
   app data.

4. Debug over SSH (`ssh umbrel@<vm-ip>`, password = your umbrelOS password):

   ```sh
   sudo cat /home/umbrel/umbrel/app-data/allod/data/umbrel-chain.env
   sudo docker ps --filter name=allod_
   sudo docker logs -f allod_ldk-server_1
   ```

If image pulls hang with "TLS handshake timeout" while the host pulls fine, the host
probably routes VM traffic through a VPN with a small MTU. Clamp TCP MSS on the host:
`sudo iptables -t mangle -A FORWARD -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu`.

## Before submitting upstream

- Images must be multi-arch (amd64 and arm64, for Raspberry Pi).
- Pin images from a release tag instead of a nightly.
- Drop `icon` (the Umbrel team hosts official icons) and add the gallery/PR fields.
- Run `npm run lint:apps -- allod --check-images` in a `getumbrel/umbrel-apps` checkout.
