# Allod developer commands. Production deploys use deploy/compose.yaml directly (SPEC §2.3).

compose := "docker compose -f deploy/compose.yaml -f deploy/compose.regtest.yaml" + (if env("CI_DOCKER_CACHE", "") == "1" { " -f deploy/compose.ci.yaml" } else { "" }) + " --env-file deploy/regtest.env"
chains := "bundled bundled-rest electrum esplora"
# One-off commands use the tools image directly: `compose run` would rebuild it every time.
tools := "docker run --rm -i --network allod-regtest_default -v " + justfile_directory() + "/deploy/.dev:/dev-secrets:ro -e BITCOIND_RPC_URL=http://bitcoind:8332 -e BITCOIND_RPC_USER=ldk -e BITCOIND_RPC_PASS=ldk-regtest ghcr.io/frnandu/allod-tools:regtest"

# List recipes
default:
    @just --list

# --- Rust / web -------------------------------------------------------------------

# Format, lint and test everything that does not need Docker
check: fmt-check clippy test web-lint test-images trim

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# pnpm always runs from web/, through corepack: corepack picks the pnpm version from the
# package.json closest to the working directory, so from the repository root it would take
# the latest release instead of the one pinned in web/package.json and refuse to run
# (ERR_PNPM_BAD_PM_VERSION: "Corepack invoked pnpm with this version").

# Unit tests (Rust + web)
test: web-install
    cargo test --workspace
    cd web && corepack pnpm test

web-install:
    cd web && corepack pnpm install --frozen-lockfile

web-lint: web-install
    cd web && corepack pnpm lint

web-build: web-install
    cd web && corepack pnpm build

# Run the Vite dev server against a Allod on 127.0.0.1:8029
web-dev: web-install
    cd web && corepack pnpm dev

# --- Regtest stack ----------------------------------------------------------------

# Build all images of the regtest stack
build:
    {{compose}} --profile tools build

# Start the regtest stack; chain = bundled | bundled-rest | electrum | esplora
up chain="bundled": _dev-dirs
    ALICE_CHAIN={{chain}} {{compose}} up -d --build
    @echo "waiting for bootstrap (funding nodes, opening channels)..."
    @scripts/wait-bootstrap.sh {{compose}}
    @{{compose}} logs --no-log-prefix bootstrap | tail -5

# Start the regtest stack WITHOUT dev auto-setup, then set Allod up yourself, e.g.
#   just up-manual
#   just allod setup --owner npub1...      (or use the wizard at http://127.0.0.1:18029)
#   just finish-up
up-manual chain="bundled": _dev-dirs
    ALICE_CHAIN={{chain}} ALLOD_DEV_AUTO_SETUP=false {{compose}} up -d --build bitcoind electrs relay relay2 ldk-bob ldk-carol ldk-server allod bootstrap
    @echo "Allod is waiting for setup: run 'just allod setup --owner <npub>' or open http://127.0.0.1:18029, then 'just finish-up'."

# After a manual setup: wait for bootstrap (funding, channels), then start the miner
finish-up:
    @scripts/wait-bootstrap.sh {{compose}}
    @{{compose}} up -d --no-build miner
    @{{compose}} logs --no-log-prefix bootstrap | tail -5

# Run the allod CLI inside the regtest Allod container, e.g. `just allod owner list`
allod *args:
    {{compose}} exec allod allod {{args}}

# Everything except Allod, for `just run` on the host
infra chain="bundled": _dev-dirs
    ALICE_CHAIN={{chain}} {{compose}} up -d --build bitcoind ldk-server ldk-bob ldk-carol electrs relay relay2 bootstrap miner
    @scripts/wait-bootstrap.sh {{compose}}

# Same as `just infra`, but reuse the images that are already built (no Docker build).
# Does not wait for bootstrap: that needs Allod, so `just run` finishes it (see below).
infra-fast chain="bundled": _dev-dirs
    ALICE_CHAIN={{chain}} {{compose}} up -d --no-build bitcoind ldk-server ldk-bob ldk-carol electrs relay relay2 bootstrap
    @for _ in $(seq 1 60); do [ -f deploy/.dev/alice.env ] && break; sleep 2; done
    @test -f deploy/.dev/alice.env || { echo "alice.env did not appear: check 'just logs bootstrap'" >&2; exit 1; }
    @echo "infra up. bootstrap is waiting for Allod: start 'just run' in another terminal."

# Run Allod on the host against `just infra` (UI: http://127.0.0.1:18029, or `just web-dev`).
# Starts the fast infra if it is not up yet and, in the background, waits for bootstrap
# (Alice's phrase comes from this process) and then starts the miner.
run:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f deploy/.dev/alice.env ]; then
      just infra-fast
    fi
    set -a; source deploy/.dev/alice.env; set +a
    export ALLOD_LDK_SERVER_CERT_FILE="$PWD/$ALLOD_LDK_SERVER_CERT_FILE"
    export ALLOD_LDK_SERVER_MACAROON_FILE="$PWD/$ALLOD_LDK_SERVER_MACAROON_FILE"
    export ALLOD_DATA_DIR="$PWD/deploy/.dev/allod-host"
    export ALLOD_HANDOFF_DIR="$PWD/deploy/.dev/handoff"
    export ALLOD_LISTEN=127.0.0.1:18029
    if (ss -ltn 2>/dev/null || netstat -ltn) | grep -q '127\.0\.0\.1:18029'; then
      echo "127.0.0.1:18029 is already in use: another 'just run' is probably still alive." >&2
      exit 1
    fi
    # This process hands Alice her recovery phrase, so bootstrap can only finish after it
    # starts; finish the stack (miner) in the background while Allod serves.
    ( scripts/wait-bootstrap.sh {{compose}} \
        && {{compose}} up -d --no-build miner \
        && echo "regtest stack ready" ) &
    cargo run -p allod -- serve

# Stop the stack, keep data
down:
    {{compose}} --profile tools down --remove-orphans

# Stop the stack and delete all regtest data
reset:
    #!/usr/bin/env bash
    set -euo pipefail
    if (ss -ltn 2>/dev/null || netstat -ltn) | grep -q '127\.0\.0\.1:18029'; then
      echo "Allod is still listening on 127.0.0.1:18029: stop 'just run' before 'just reset'." >&2
      exit 1
    fi
    {{compose}} --profile tools down -v --remove-orphans
    rm -rf deploy/.dev

logs service="":
    {{compose}} logs -f {{service}}

ps:
    {{compose}} ps -a

# Mine n regtest blocks
mine n="1":
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; mine {{n}}; btc getblockcount'

# Send BTC on-chain to alice, bob or carol
fund node amount="1":
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; addr=$(ldk {{node}} onchain-receive | jq -r .address); btc -rpcwallet=miner sendtoaddress $addr {{amount}}; mine 6'

# ldk-server-cli against Alice (Allod' node), e.g. `just alice list-channels`
alice *args:
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; ldk alice "$@"' _ {{args}}

# ldk-server-cli against Bob, e.g. `just bob bolt11-receive 50000sat`
bob *args:
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; ldk bob "$@"' _ {{args}}

# ldk-server-cli against Carol
carol *args:
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; ldk carol "$@"' _ {{args}}

# bitcoin-cli against the regtest node
btc *args:
    @{{tools}} bash -c 'source /usr/local/share/allod/lib.sh; btc "$@"' _ {{args}}

# Create a NWC connection on the regtest Allod and print its link, e.g.
#   URI=$(just nwc-uri)            or  just nwc-uri --scopes get_balance --budget-sats 0
nwc-uri *args:
    @cargo run -q -p allod-devtools -- nwc-uri {{args}}

# Call a NWC connection, e.g.  just nwc "$URI" get_balance   /   just nwc "$URI" make_invoice '{"amount":1000}'
nwc uri method params="{}":
    @cargo run -q -p allod-devtools -- nwc --uri '{{uri}}' {{method}} '{{params}}'

# Check every service of the running stack
doctor:
    @{{tools}} allod-devtools check \
        --bitcoind-url http://bitcoind:8332 \
        --ldk alice=ldk-server:3536,/dev-secrets/alice/tls.crt,/dev-secrets/alice/admin.macaroon \
        --ldk bob=ldk-bob:3536,/dev-secrets/bob/tls.crt,/dev-secrets/bob/admin.macaroon \
        --ldk carol=ldk-carol:3536,/dev-secrets/carol/tls.crt,/dev-secrets/carol/admin.macaroon \
        --min-channels alice=2 --min-channels bob=2 --min-channels carol=2 \
        --max-tip-lag 3 \
        --relay ws://relay:8080 --relay ws://relay2:8080 \
        --allod-url http://allod:8029 --allod-requires-ldk \
        --wait 120

# e2e tests against the running stack (SPEC §16.4): stack checks, then the API scenarios
e2e *args: doctor
    cargo test -p allod-e2e -- --ignored {{args}}
    @just trim

# bitcoind image helpers: fast sync download/loading against a flaky local server, and
# the initial sync profile sized against fake memory/cgroup files
test-images:
    images/bitcoind/test-assumeutxo.sh
    images/bitcoind/test-sync-tuning.sh

# Cargo never deletes old build output; anything still needed is rebuilt on demand.
# Drop build output not read for `days` days, and old incremental sessions
trim days="2":
    #!/usr/bin/env bash
    set -euo pipefail
    [ -d target ] || exit 0
    before=$(du -sm target | cut -f1)
    for d in target/*/deps target/*/incremental target/*/build target/*/.fingerprint; do
        [ -d "$d" ] && find "$d" -mindepth 1 -maxdepth 1 -atime +{{days}} -exec rm -rf {} +
    done
    find target -maxdepth 3 -type d -name incremental -exec sh -c 'for s in "$1"/*/; do ls -dt "$s"s-*/ 2>/dev/null | tail -n +2 | xargs -r rm -rf; done' _ {} \;
    after=$(du -sm target | cut -f1)
    echo "target/: ${before} MB -> ${after} MB"

# Fresh stack without dev auto-setup: Playwright runs the setup wizard and sign-in
# against it, then the stack must come up fully (bootstrap + doctor).
e2e-ui: _dev-dirs
    #!/usr/bin/env bash
    set -euo pipefail
    just reset >/dev/null 2>&1 || true
    mkdir -p deploy/.dev/alice deploy/.dev/bob deploy/.dev/carol deploy/.dev/handoff
    chmod a+rwx deploy/.dev deploy/.dev/alice deploy/.dev/bob deploy/.dev/carol deploy/.dev/handoff
    # Everything except the miner: it waits for bootstrap, which waits for the setup
    # that Playwright performs.
    ALLOD_DEV_AUTO_SETUP=false {{compose}} up -d --build bitcoind electrs relay relay2 ldk-bob ldk-carol ldk-server allod bootstrap
    (cd web && corepack pnpm install --frozen-lockfile >/dev/null)
    (cd web && corepack pnpm exec playwright install chromium >/dev/null)
    (cd web && ALLOD_E2E_URL=http://127.0.0.1:18029 corepack pnpm exec playwright test)
    scripts/wait-bootstrap.sh {{compose}}
    {{compose}} up -d miner
    just doctor

# Run the stack and doctor for every chain mode, resetting between runs
e2e-matrix:
    #!/usr/bin/env bash
    set -uo pipefail
    declare -A result
    for chain in {{chains}}; do
        echo "=== ALICE_CHAIN=$chain"
        just reset >/dev/null 2>&1
        if just up "$chain" && just doctor; then result[$chain]=pass; else result[$chain]=FAIL; just logs ldk-server 2>/dev/null | tail -40 || true; fi
    done
    just reset >/dev/null 2>&1
    echo; printf '%-14s %s\n' chain result
    fail=0
    for chain in {{chains}}; do printf '%-14s %s\n' "$chain" "${result[$chain]}"; [ "${result[$chain]}" = pass ] || fail=1; done
    exit $fail

# Validate the Compose files (production, all profiles, regtest overlay)
compose-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cp deploy/.env.example deploy/.env.ci
    trap 'rm -f deploy/.env.ci' EXIT
    docker compose -f deploy/compose.yaml --env-file deploy/.env.ci config -q
    COMPOSE_PROFILES=bundled,https,tor,backup,postgres,electrs docker compose -f deploy/compose.yaml --env-file deploy/.env.ci config -q
    {{compose}} config -q
    echo "compose files OK"

# Render the release bundle (SPEC §2.3.8); digests come from the release workflow
bundle version digests_file="":
    scripts/render-bundle.sh {{version}} {{digests_file}}

_dev-dirs:
    @mkdir -p deploy/.dev/alice deploy/.dev/bob deploy/.dev/carol deploy/.dev/handoff && chmod a+rwx deploy/.dev deploy/.dev/alice deploy/.dev/bob deploy/.dev/carol deploy/.dev/handoff 2>/dev/null || true
