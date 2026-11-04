# Allod for umbrelOS

`allod/` is the umbrelOS app package (SPEC §2.3.6). This directory is also a community
app store root (`umbrel-app-store.yml`, store id `allod`).

## Chain source

The package declares no app dependency. On the first start, `exports.sh` checks for
Umbrel's Bitcoin Node app (`bitcoin`):

| Bitcoin Node app | Allod uses | Containers |
|---|---|---|
| installed | that node over RPC (`10.21.21.8:8332`, its credentials and network) | `allod`, `ldk-server`, `gossip` |
| not installed | its own pruned Bitcoin Core, mainnet, `BITCOIN_PRUNE=auto` | the same three plus `bitcoind` (compose profile `bundled`) |

`hooks/pre-start` saves the choice to `data/umbrel-chain.env`. Later starts keep it, so
installing or removing the Bitcoin Node app afterwards does not move the node. To change
the source, use Allod's Settings > Bitcoin > Chain source.

The Bitcoin Node app can itself be pruned: its Settings > Optimization > Prune Old
Blocks. Allod works with a pruned node.

## Test on a local umbrelOS VM

1. Serve the store from your working tree. The server binds to the libvirt bridge, so
   only VMs on it can reach it. Every fetch snapshots `packaging/umbrel/` again, so you
   don't need to commit or push.

   ```sh
   just umbrel-store            # http://192.168.122.1:8765/allod-store.git
   ```

2. In umbrelOS, open App Store, then the `...` menu, then Community App Stores. Add
   `http://192.168.122.1:8765/allod-store.git`, then install Allod from the "Allod" store.

3. Iterate. umbrelOS only offers an update when `version` in `umbrel-app.yml` changes,
   so bump it, then refresh the store (or wait for the periodic pull). You can also
   uninstall and install again.

   To test the other chain mode, uninstall Allod, install or remove the Bitcoin Node
   app, and install Allod again. Uninstalling deletes the saved choice along with the
   app data.

4. Debug over SSH (`ssh umbrel@<vm-ip>`, password = your umbrelOS password):

   ```sh
   sudo cat /home/umbrel/umbrel/app-data/allod/data/umbrel-chain.env
   sudo docker ps --filter name=allod_
   sudo docker logs -f allod_ldk-server_1
   ```

## Before submitting upstream

- Pin images from a release tag instead of a nightly.
- Drop `icon` (the Umbrel team hosts official icons) and add the gallery/PR fields.
- Run `npm run lint:apps -- allod --check-images` in a `getumbrel/umbrel-apps` checkout.
