# Allod — agent orientation

Allod is a Rust wallet service for an ldk-server Lightning node. It implements full Nostr Wallet Connect (NIP-47 plus every NWC extension except keysend (NWC-04), BOLT12 included), isolated accounts (Alby Hub's sub-wallets) on a double-entry ledger, and a REST API for a React web UI.

- **Read `SPEC.md` first.** It is normative. Do not change behaviour that the spec defines without updating `SPEC.md` in the same change.
- Work one milestone at a time (SPEC §17). A milestone is done only when its listed tests pass.
- Reference code on this machine is listed in SPEC §0 (Alby Hub, ldk-server, NWC specs).
- Out of scope: keysend/spontaneous payments (no NWC-04). License: MIT.
- No unlock step: Allod trusts its host (SPEC §3.3). Secrets live in `{data_dir}/secrets/` files, never in the database. Owners sign in with Nostr (NIP-98).
- Money rules (SPEC §6–7) are the highest priority: never mark a payment settled or failed without backend confirmation, always validate balance and budget under the ledger lock, and always scope data access by app.
- Deployment: `deploy/compose.yaml` is the production bundle (VPS, Umbrel, StartOS wrap it; SPEC §2.3). `just up` runs it on regtest with test counterparties; `just e2e` runs integration tests against it.
- Checks before you push: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm -C web lint test build`.
