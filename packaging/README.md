# Platform packages

- **Umbrel**: `../allod/` (app) and `../umbrel-app-store.yml` (community app store). Users add
  `https://github.com/frnandu/allod` under App Store → Community App Stores.
- **StartOS**: `startos/` (StartOS 0.4, start-sdk 4). See below.

## StartOS

### Build

Needs Docker, Node 22+, make, jq and [`start-cli`](https://docs.start9.com/packaging/environment-setup.html).

```sh
cp ~/.startos/id.key.pem .startos/build.key.pem   # once: your signing key (start-cli init-key)
cd startos && npm ci && make                      # → allod_x86_64.s9pk
```

`.startos/config.yaml` marks the packaging workspace and names the default StartOS host.

### Install on a box

Sideload through the StartOS UI (Sideload), or:

```sh
start-cli auth login                              # against host.default
start-cli package install --trust -s startos/allod_x86_64.s9pk
```

### Registry (the StartOS "community app store")

StartOS installs from **registries**. A registry is the `startos-registry` service (install
it from the Start9 marketplace on any StartOS box) or the `start-registry` server binary.
Users add its URL under **System → Registries → Add registry**; StartOS then trusts the
signing keys the registry offers and lists its packages in the Marketplace.

Set up once (from a workstation with `start-cli`, `R` = the registry's Web API address):

1. In StartOS, run the registry's **Configure Registry** and **Add Administrator** actions
   (paste `start-cli pubkey`).
2. Offer your key to users: `start-cli -r $R registry admin signer list`, then
   `start-cli -r $R registry info set-trust-signers <SIGNER_ID>`.
3. `start-cli -r $R registry package category add bitcoin Bitcoin`

Publish a version (the registry indexes a URL; it does not store the file):

```sh
# upload allod_x86_64.s9pk somewhere public, e.g. a GitHub release asset, then:
start-cli -r $R registry package add startos/allod_x86_64.s9pk --url <public URL>
start-cli -r $R registry package category add-package bitcoin allod
```

Publish your signing public key (`start-cli pubkey`) somewhere users already trust (the
GitHub README) and link it in the registry description.

### Local test VM

`scripts/startos-vm.md` documents the libvirt VM used for testing.
