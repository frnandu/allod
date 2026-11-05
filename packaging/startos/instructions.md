# Allod

## Documentation

- [Allod repository](https://github.com/frnandu/allod): project README and specification
- [Nostr Wallet Connect](https://nwc.dev): the protocol apps use to connect to your wallet

## What you get on StartOS

A Lightning node (ldk-server) on top of your Bitcoin Core, a web UI to manage it, Nostr
Wallet Connect for your apps, and isolated sub-wallets.

## Getting set up

1. Bitcoin: if the Bitcoin Core service is installed when Allod first starts, Allod uses it.
   Otherwise Allod runs its own pruned Bitcoin node. The Lightning node starts after the
   Bitcoin sync finishes, which can take hours to days.
2. Run the **Open setup** action, click its LAN or Tor link, then sign in with a Nostr
   signer (browser extension or remote signer). Allod removes the one-time secret from
   the address bar immediately.
3. Create your node. Write the 24-word recovery phrase on paper: it restores on-chain funds
   only, not Lightning channels.
4. Set up a channel backup in Allod's Settings. A StartOS backup is a last resort.
5. Lost your Nostr key? Run the **Add owner key** action with a new npub.
