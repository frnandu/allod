#!/bin/sh
# Allod VPS installer (SPEC §2.3.5).
#
# Sets up the Allod Docker Compose stack from a release bundle and prints how to start
# it. It never starts the stack itself and never touches an existing .env.
#
# Runs as any user who can use Docker: as root it installs to /opt/allod, otherwise to
# ~/allod. sudo is only needed to install Docker when it is missing.
#
# Recommended: download, read, verify, run:
#   curl -fsSLO https://github.com/frnandu/allod/releases/latest/download/install.sh
#   curl -fsSLO https://github.com/frnandu/allod/releases/latest/download/SHA256SUMS
#   sha256sum -c --ignore-missing SHA256SUMS
#   less install.sh
#   sh install.sh
# Shortcut:
#   curl -fsSL https://github.com/frnandu/allod/releases/latest/download/install.sh | sh
#
# Options:
#   --version VER       install release VER (e.g. 0.1.1) instead of the latest
#   --nightly           install the latest nightly build (untested; not for real funds)
#   --dir DIR           install directory (default /opt/allod as root, else ~/allod)
#   --bundle FILE       use a local allod-stack-<ver>.tar.gz instead of downloading;
#                       verified against SHA256SUMS next to it when present
#   --non-interactive   ask nothing; settings come from --env-file and defaults
#   --env-file FILE     KEY=VALUE settings applied over .env.example
#   -h, --help          show this help
set -eu

REPO_URL=${ALLOD_REPO_URL:-https://github.com/frnandu/allod}
DIR=
VERSION=
BUNDLE=
INTERACTIVE=1
_yn='' _c='' # set by ask() through eval
ENV_IN=

say() { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
  cat <<'EOF2'
Usage: install.sh [options]
  --version VER       install release VER (e.g. 0.1.1) instead of the latest
  --nightly           install the latest nightly build (untested; not for real funds)
  --dir DIR           install directory (default /opt/allod as root, else ~/allod)
  --bundle FILE       use a local allod-stack-<ver>.tar.gz instead of downloading
  --non-interactive   ask nothing; settings come from --env-file and defaults
  --env-file FILE     KEY=VALUE settings applied over .env.example
EOF2
}

while [ $# -gt 0 ]; do
  case $1 in
    --version) VERSION=${2:?--version needs a value}; VERSION=${VERSION#v}; shift 2 ;;
    --nightly) VERSION=nightly; shift ;;
    --dir) DIR=${2:?--dir needs a value}; shift 2 ;;
    --bundle) BUNDLE=${2:?--bundle needs a file}; shift 2 ;;
    --non-interactive) INTERACTIVE=0; shift ;;
    --env-file) ENV_IN=${2:?--env-file needs a file}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done

# --- prompts (read from the terminal, so `curl | sh` works) ---------------------------

if [ "$INTERACTIVE" = 1 ]; then
  { : </dev/tty; } 2>/dev/null || die "no terminal to ask questions on; use --non-interactive --env-file FILE"
fi

# ask VAR "question" [default]
ask() {
  _def=${3:-}
  if [ "$INTERACTIVE" = 0 ]; then eval "$1=\$_def"; return; fi
  if [ -n "$_def" ]; then printf '%s [%s]: ' "$2" "$_def" >/dev/tty; else printf '%s: ' "$2" >/dev/tty; fi
  IFS= read -r _ans </dev/tty || _ans=
  [ -n "$_ans" ] || _ans=$_def
  eval "$1=\$_ans"
}

# ask_secret VAR "question" (no echo)
ask_secret() {
  if [ "$INTERACTIVE" = 0 ]; then eval "$1="; return; fi
  printf '%s: ' "$2" >/dev/tty
  stty -echo </dev/tty 2>/dev/null || true
  IFS= read -r _ans </dev/tty || _ans=
  stty echo </dev/tty 2>/dev/null || true
  printf '\n' >/dev/tty
  eval "$1=\$_ans"
}

# confirm "question" y|n -> exit status
confirm() {
  if [ "$INTERACTIVE" = 0 ]; then [ "$2" = y ]; return; fi
  while :; do
    if [ "$2" = y ]; then ask _yn "$1 (Y/n)" y; else ask _yn "$1 (y/N)" n; fi
    case $_yn in [Yy]|[Yy][Ee][Ss]) return 0 ;; [Nn]|[Nn][Oo]) return 1 ;; esac
  done
}

# choose VAR "question" default opt1 "desc1" opt2 "desc2" ...
choose() {
  _var=$1 _q=$2 _def=$3; shift 3
  if [ "$INTERACTIVE" = 0 ]; then eval "$_var=\$_def"; return; fi
  printf '%s\n' "$_q" >/dev/tty
  _i=0 _opts=
  while [ $# -gt 0 ]; do
    _i=$((_i + 1)); _opts="$_opts $1"
    if [ "$1" = "$_def" ]; then _mark='*'; else _mark=' '; fi
    printf ' %s %d) %-18s %s\n' "$_mark" "$_i" "$1" "$2" >/dev/tty
    shift 2
  done
  while :; do
    ask _c "Choice (number or name)" "$_def"
    _i=0
    for _o in $_opts; do
      _i=$((_i + 1))
      if [ "$_c" = "$_i" ] || [ "$_c" = "$_o" ]; then eval "$_var=\$_o"; return; fi
    done
    say "Pick one of:$_opts"
  done
}

# --- .env editing ------------------------------------------------------------------------

# set_env FILE KEY VALUE: replaces "KEY=..." or a commented "#KEY=..." / "# KEY=..." line,
# else appends. Values with characters .env would misread are single-quoted.
set_env() {
  case $3 in
    *"'"*) die "$2: values cannot contain a single quote" ;;
    *[!A-Za-z0-9_./:@,+=-]*) _v="'$3'" ;;
    *) _v=$3 ;;
  esac
  K=$2 V=$_v awk '
    BEGIN { k = ENVIRON["K"]; v = ENVIRON["V"]; done = 0 }
    !done && ($0 ~ "^" k "=" || $0 ~ "^# ?" k "=") { print k "=" v; done = 1; next }
    { print }
    END { if (!done) print k "=" v }
  ' "$1" >"$1.tmp"
  cat "$1.tmp" >"$1" && rm -f "$1.tmp"
}

# get_env FILE KEY: value of an uncommented KEY line, quotes stripped
get_env() {
  sed -n "s/^$2=//p" "$1" | tail -n 1 | sed -e "s/^'\(.*\)'\$/\1/" -e 's/^"\(.*\)"$/\1/'
}

secret() { od -An -tx1 -N24 /dev/urandom | tr -d ' \n'; }

# --- host checks -----------------------------------------------------------------------

ME=${USER:-}; [ -n "$ME" ] || ME=$(id -un 2>/dev/null | head -n 1); [ -n "$ME" ] || ME=$(id -u)
if [ "$(id -u)" = 0 ]; then
  SUDO=
  [ -n "$DIR" ] || DIR=/opt/allod
else
  SUDO=sudo
  [ -n "$DIR" ] || DIR=${HOME:?HOME is not set}/allod
fi

OS_ID='' OS_VER='' OS_CODENAME=''
if [ -r /etc/os-release ]; then
  # In a subshell: os-release sets VERSION, NAME, ID..., which would clobber ours.
  # shellcheck disable=SC1091
  os_field() { (. /etc/os-release; eval "printf '%s' \"\${$1:-}\""); }
  OS_ID=$(os_field ID) OS_VER=$(os_field VERSION_ID)
  OS_CODENAME=$(os_field UBUNTU_CODENAME)
  [ -n "$OS_CODENAME" ] || OS_CODENAME=$(os_field VERSION_CODENAME)
fi
os_ok=0
case $OS_ID in
  debian) [ "${OS_VER%%.*}" -ge 12 ] 2>/dev/null && os_ok=1 ;;
  ubuntu) [ "${OS_VER%%.*}" -ge 22 ] 2>/dev/null && os_ok=1 ;;
esac
[ "$os_ok" = 1 ] || warn "supported hosts are Debian 12+ and Ubuntu 22.04+ (found ${OS_ID:-unknown} ${OS_VER}); continuing"

case $(uname -m) in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) die "unsupported CPU architecture $(uname -m) (amd64 or arm64 only)" ;;
esac

for t in tar sha256sum awk sed od; do have "$t" || die "missing $t"; done
if have curl; then fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif have wget; then fetch() { wget -q -O "$2" "$1"; }
else die "need curl or wget"; fi

MEM_MB=$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo 2>/dev/null || echo 0)
mkdir -p "$DIR"
DISK_GB=$(df -Pk "$DIR" | awk 'NR == 2 { print int($4 / 1048576) }')
say "Host: ${OS_ID:-unknown} ${OS_VER}, $(uname -m), ${MEM_MB} MiB RAM, ${DISK_GB} GiB free in $DIR"

port_busy() {
  have ss || return 1
  [ -n "$(ss -Hltn "sport = :$1" 2>/dev/null)" ]
}

# --- Docker ------------------------------------------------------------------------------

install_docker() {
  case $OS_ID in debian|ubuntu) ;; *) die "install Docker Engine and the compose plugin yourself: https://docs.docker.com/engine/install/" ;; esac
  [ -n "$OS_CODENAME" ] || die "cannot tell the $OS_ID release codename; install Docker yourself"
  if [ -n "$SUDO" ]; then
    have sudo || die "installing Docker needs root: run this as root, or install sudo"
    say "Installing Docker needs root; sudo may ask for your password."
  fi
  say "Installing Docker Engine from download.docker.com..."
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get update -q
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -q ca-certificates curl
  $SUDO install -m 0755 -d /etc/apt/keyrings
  curl -fsSL "https://download.docker.com/linux/$OS_ID/gpg" | $SUDO tee /etc/apt/keyrings/docker.asc >/dev/null
  $SUDO chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/$OS_ID $OS_CODENAME stable" \
    | $SUDO tee /etc/apt/sources.list.d/docker.list >/dev/null
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get update -q
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -q docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
  $SUDO systemctl enable --now docker
  if [ -n "$SUDO" ]; then
    $SUDO usermod -aG docker "$ME"
    say ""
    say "Docker is installed and $ME was added to the docker group."
    say "Log out and back in (or run: newgrp docker), then run install.sh again."
    exit 0
  fi
}

if ! have docker || ! docker compose version >/dev/null 2>&1; then
  say "Docker Engine with the compose plugin is not installed."
  if confirm "Install it from Docker's apt repository now?" y; then install_docker
  else die "Allod needs Docker Engine and the compose plugin"; fi
fi
if ! docker_err=$(docker info 2>&1 >/dev/null); then
  case $docker_err in
    *"permission denied"*)
      die "$ME cannot use Docker. Either add $ME to the docker group (sudo usermod -aG docker $ME, then log in again; members of that group are effectively root on this host), set up rootless Docker (https://docs.docker.com/engine/security/rootless/), or run this as root" ;;
    *) die "cannot reach the Docker daemon: $docker_err" ;;
  esac
fi
ROOTLESS=0
case $(docker info -f '{{range .SecurityOptions}}{{.}} {{end}}' 2>/dev/null) in *rootless*) ROOTLESS=1 ;; esac

# --- release bundle ----------------------------------------------------------------------

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if [ -n "$BUNDLE" ]; then
  [ -f "$BUNDLE" ] || die "no such file $BUNDLE"
  sums="$(dirname "$BUNDLE")/SHA256SUMS"
  name=$(basename "$BUNDLE")
  if [ -f "$sums" ] && grep -q " \*\{0,1\}$name\$" "$sums"; then
    (cd "$(dirname "$BUNDLE")" && grep " \*\{0,1\}$name\$" SHA256SUMS | sha256sum -c -) >&2 || die "checksum mismatch for $BUNDLE"
  else
    warn "no SHA256SUMS entry next to $BUNDLE; not verified"
  fi
  cp "$BUNDLE" "$WORK/$name"
else
  case $VERSION in
    '') base="$REPO_URL/releases/latest/download" ;;
    nightly) base="$REPO_URL/releases/download/nightly" ;;
    *) base="$REPO_URL/releases/download/v$VERSION" ;;
  esac
  say "Downloading release from $base"
  fetch "$base/SHA256SUMS" "$WORK/SHA256SUMS" || die "cannot download $base/SHA256SUMS"
  name=$(awk '{ sub(/^\*/, "", $2) } $2 ~ /^allod-stack-.*\.tar\.gz$/ { print $2; exit }' "$WORK/SHA256SUMS")
  [ -n "$name" ] || die "SHA256SUMS lists no allod-stack bundle"
  fetch "$base/$name" "$WORK/$name" || die "cannot download $base/$name"
  (cd "$WORK" && grep " \*\{0,1\}$name\$" SHA256SUMS | sha256sum -c -) >&2 || die "checksum mismatch for $name"
fi
VERSION=${name#allod-stack-}; VERSION=${VERSION%.tar.gz}
case $VERSION in nightly-*) warn "$VERSION is a nightly build: untested, for test networks, not for real funds" ;; esac
tar -C "$WORK" -xzf "$WORK/$name"
SRC="$WORK/allod-stack-$VERSION"
{ [ -f "$SRC/compose.yaml" ] && [ -f "$SRC/.env.example" ]; } || die "$name is not an Allod stack bundle"

# Nightly builds started by hand can be amd64 only: check the pinned images have this
# host's architecture before writing anything.
awk '$1 == "image:" && $2 ~ /@sha256:/ { print $2 }' "$SRC/compose.yaml" | sort -u >"$WORK/refs"
while IFS= read -r ref; do
  if ! manifest=$(docker manifest inspect "$ref" 2>/dev/null); then
    warn "cannot inspect $ref; not checking its architectures"
    continue
  fi
  case $manifest in *'"manifests"'*) ;; *) continue ;; esac
  if ! printf '%s' "$manifest" | grep -q "\"architecture\": *\"$ARCH\""; then
    die "$VERSION has no linux/$ARCH image for ${ref%@*} (an amd64-only build). Use a release, or a nightly built for $ARCH."
  fi
done <"$WORK/refs"

# --- install files -----------------------------------------------------------------------

ENV="$DIR/.env"
EXISTING=0
[ -f "$ENV" ] && EXISTING=1

for f in compose.yaml Caddyfile litestream.yml .env.example; do
  [ -f "$SRC/$f" ] || continue
  if [ -f "$DIR/$f" ] && ! cmp -s "$SRC/$f" "$DIR/$f"; then cp -p "$DIR/$f" "$DIR/$f.bak"; fi
  install -m 0644 "$SRC/$f" "$DIR/$f"
done

if [ "$EXISTING" = 1 ]; then
  # Never regenerate an existing configuration: it holds the stack's settings and
  # passwords. Only move it to the new release's images.
  set_env "$ENV" ALLOD_VERSION "$VERSION"
  chmod 0600 "$ENV"
  say ""
  say "Existing installation in $DIR: kept your .env, updated the stack files to $VERSION."
  say "Edit $ENV by hand to change settings (compare with $DIR/.env.example)."
else
  umask 077
  cp "$SRC/.env.example" "$ENV.new"
  chmod 0600 "$ENV.new"

  if [ -n "$ENV_IN" ]; then
    [ -f "$ENV_IN" ] || die "no such file $ENV_IN"
    while IFS= read -r line || [ -n "$line" ]; do
      case $line in ''|'#'*) continue ;; *=*) ;; *) continue ;; esac
      k=${line%%=*} v=${line#*=}
      v=$(printf '%s' "$v" | sed -e "s/^'\(.*\)'\$/\1/" -e 's/^"\(.*\)"$/\1/')
      set_env "$ENV.new" "$k" "$v"
    done <"$ENV_IN"
  fi

  # Defaults below come from the env file (if any), else from .env.example.
  cur() { get_env "$ENV.new" "$1"; }

  if [ "$INTERACTIVE" = 1 ]; then
    say ""
    say "== Allod $VERSION setup =="
    say "Press Enter to keep the default shown in brackets."
    say ""

    choose NETWORK "Bitcoin network:" "$(cur ALLOD_NETWORK)" \
      bitcoin "mainnet, real funds" \
      signet "test network, coins have no value" \
      testnet4 "test network, coins have no value"
    set_env "$ENV.new" ALLOD_NETWORK "$NETWORK"

    say ""
    choose CHAIN "Chain source:" "$(cur ALLOD_CHAIN)" \
      bundled "own pruned Bitcoin Core in this stack: 2 vCPU, 4 GB RAM, 80 GB SSD; sync takes hours to days" \
      external-bitcoind "a Bitcoin Core node you already run (RPC access)" \
      electrum "public or own Electrum server: 1 GB RAM, 20 GB; less private" \
      esplora "public or own Esplora server: 1 GB RAM, 20 GB; less private"
    set_env "$ENV.new" ALLOD_CHAIN "$CHAIN"

    case $CHAIN in
      bundled)
        if confirm "Keep the full blockchain (unpruned, about 800 GB disk)?" n; then
          set_env "$ENV.new" BITCOIN_PRUNE 0
        fi ;;
      external-bitcoind)
        say "Docker containers cannot reach the host's 127.0.0.1; use an address they can reach."
        ask v "Bitcoin Core RPC address (host:port)" "$(cur BITCOIND_RPC_ADDRESS | sed 's/^bitcoind:.*//')"
        [ -n "$v" ] || die "an RPC address is required"
        set_env "$ENV.new" BITCOIND_RPC_ADDRESS "$v"
        ask v "RPC user" "$(cur BITCOIND_RPC_USER)"
        set_env "$ENV.new" BITCOIND_RPC_USER "$v"
        ask_secret v "RPC password"
        [ -n "$v" ] || die "an RPC password is required"
        set_env "$ENV.new" BITCOIND_RPC_PASS "$v"
        say "Allod also reads sync progress over RPC (getblockchaininfo, getnetworkinfo, estimatesmartfee)."
        if confirm "Use a separate read-only RPC user for that?" n; then
          ask v "Read-only RPC user" "allod-ro"
          set_env "$ENV.new" BITCOIND_RO_RPC_USER "$v"
          ask_secret v "Read-only RPC password"
          set_env "$ENV.new" BITCOIND_RO_RPC_PASS "$v"
        else
          set_env "$ENV.new" BITCOIND_RO_RPC_USER "$(cur BITCOIND_RPC_USER)"
          set_env "$ENV.new" BITCOIND_RO_RPC_PASS "$(cur BITCOIND_RPC_PASS)"
        fi ;;
      electrum)
        case $NETWORK in bitcoin) d=ssl://electrum.getalbypro.com:50002 ;; *) d= ;; esac
        ask v "Electrum server URL (ssl://host:port or tcp://host:port)" "$d"
        [ -n "$v" ] || die "an Electrum URL is required"
        set_env "$ENV.new" LDK_ELECTRUM_URL "$v" ;;
      esplora)
        case $NETWORK in
          bitcoin) d=https://electrs.getalbypro.com ;;
          signet) d=https://mempool.space/signet/api ;;
          testnet4) d=https://mempool.space/testnet4/api ;;
        esac
        ask v "Esplora server URL" "$d"
        [ -n "$v" ] || die "an Esplora URL is required"
        set_env "$ENV.new" LDK_ESPLORA_URL "$v" ;;
    esac

    say ""
    ask v "Lightning node alias" "$(cur ALLOD_ALIAS)"
    set_env "$ENV.new" ALLOD_ALIAS "$v"
    say "Announcing a public address lets other nodes find and connect to yours (needed"
    say "to receive channels from peers). Leave empty for a private node."
    ask v "Public address to announce (e.g. 203.0.113.7:9735, empty for none)" ""
    set_env "$ENV.new" ALLOD_ANNOUNCE_ADDRESSES "$v"

    say ""
    say "HTTPS: with a domain pointing at this server, Caddy serves the web UI with a"
    say "Let's Encrypt certificate. Without one, the UI is reachable only over an SSH tunnel"
    say "or Tor, never as plain HTTP on the public IP."
    ask DOMAIN "Domain (empty for none)" "$(cur ALLOD_DOMAIN)"
    PROFILES=
    case $CHAIN in bundled) PROFILES=bundled ;; esac
    if [ -n "$DOMAIN" ]; then
      ask EMAIL "Email for Let's Encrypt" "$(cur ACME_EMAIL)"
      set_env "$ENV.new" ALLOD_DOMAIN "$DOMAIN"
      set_env "$ENV.new" ACME_EMAIL "$EMAIL"
      set_env "$ENV.new" ALLOD_PUBLIC_URL "https://$DOMAIN"
      PROFILES="$PROFILES,https"
      if have getent && ! getent hosts "$DOMAIN" >/dev/null 2>&1; then
        warn "$DOMAIN does not resolve yet; point its DNS A/AAAA record at this server before starting"
      fi
    fi

    say ""
    if confirm "Publish the web UI and Lightning port as Tor onion services?" "$([ -n "$DOMAIN" ] && echo n || echo y)"; then
      PROFILES="$PROFILES,tor"
    fi

    say ""
    say "Backups: Litestream continuously copies channel state and the Allod database"
    say "to S3-compatible storage, SFTP or a directory."
    if confirm "Configure a backup target now?" y; then
      ask v "Replica URL (s3://bucket/path, sftp://user@host/path, file:///path)" "$(cur LITESTREAM_REPLICA_URL)"
      if [ -n "$v" ]; then
        set_env "$ENV.new" LITESTREAM_REPLICA_URL "$v"
        case $v in s3://*)
          ask k "S3 access key id" "$(cur LITESTREAM_ACCESS_KEY_ID)"
          set_env "$ENV.new" LITESTREAM_ACCESS_KEY_ID "$k"
          ask_secret k "S3 secret access key"
          set_env "$ENV.new" LITESTREAM_SECRET_ACCESS_KEY "$k" ;;
        esac
        PROFILES="$PROFILES,backup"
      fi
    fi
    case $PROFILES in
      *backup*) ;;
      *)
        say ""
        say "WARNING: no backup target. The recovery phrase restores on-chain funds only."
        say "If this server's disk is lost, funds in open channels can be lost too."
        say "You can add LITESTREAM_* settings and the backup profile to $ENV later." ;;
    esac

    set_env "$ENV.new" COMPOSE_PROFILES "${PROFILES#,}"
  else
    # Non-interactive: COMPOSE_PROFILES must match the chain mode.
    case $(cur ALLOD_CHAIN) in
      bundled|bundled-rest) ;;
      *) p=$(cur COMPOSE_PROFILES | tr ',' '\n' | grep -vx bundled | paste -sd, -)
         set_env "$ENV.new" COMPOSE_PROFILES "$p" ;;
    esac
  fi

  # Secrets: generate any left at the example placeholder or empty.
  for k in BITCOIND_RPC_PASS BITCOIND_RO_RPC_PASS; do
    case $(cur "$k") in ''|change-me|change-me-too) set_env "$ENV.new" "$k" "$(secret)" ;; esac
  done
  case ",$(cur COMPOSE_PROFILES)," in *,postgres,*)
    if [ -z "$(cur POSTGRES_PASSWORD)" ]; then
      pw=$(secret)
      set_env "$ENV.new" POSTGRES_PASSWORD "$pw"
      set_env "$ENV.new" ALLOD_DATABASE_URL "postgres://allod:$pw@postgres:5432/allod"
      set_env "$ENV.new" LDK_POSTGRES_URL "postgresql://allod:$pw@postgres:5432"
    fi ;;
  esac

  # Small hosts: leave the OS and the rest of the stack room next to bitcoind.
  case ",$(cur COMPOSE_PROFILES)," in *,bundled,*)
    if [ "$MEM_MB" -gt 0 ] && [ "$MEM_MB" -lt 3072 ] && [ "$(cur BITCOIN_MEMORY_LIMIT)" = 2g ]; then
      set_env "$ENV.new" BITCOIN_MEMORY_LIMIT 1g
    fi ;;
  esac

  set_env "$ENV.new" ALLOD_VERSION "$VERSION"
  mv "$ENV.new" "$ENV"
  chmod 0600 "$ENV"
fi

# --- validate and report -----------------------------------------------------------------

(cd "$DIR" && docker compose config -q) || die "the generated configuration is invalid; check $ENV"

profiles=",$(get_env "$ENV" COMPOSE_PROFILES),"
chain_mode=$(get_env "$ENV" ALLOD_CHAIN)
domain=$(get_env "$ENV" ALLOD_DOMAIN)
lnport=$(get_env "$ENV" ALLOD_LIGHTNING_PORT); lnport=${lnport:-9735}

case $profiles in *,bundled,*) need_mem=3500 need_disk=80 ;; *) need_mem=900 need_disk=20 ;; esac
[ "$(get_env "$ENV" BITCOIN_PRUNE)" = 0 ] && case $profiles in *,bundled,*) need_disk=900 ;; esac
[ "$MEM_MB" -ge "$need_mem" ] || warn "${MEM_MB} MiB RAM is below the recommended $((need_mem / 1000 + 1)) GB for this setup"
[ "$DISK_GB" -ge "$need_disk" ] || warn "${DISK_GB} GiB free is below the recommended $need_disk GB for this setup"

busy=
for p in $lnport $(case $profiles in *,https,*) echo 80 443 ;; esac); do
  port_busy "$p" && busy="$busy $p"
done
[ -z "$busy" ] || warn "port(s)$busy already in use on this host; free them before starting"

if [ "$ROOTLESS" = 1 ]; then
  case $profiles in *,https,*)
    low=$(cat /proc/sys/net/ipv4/ip_unprivileged_port_start 2>/dev/null || echo 1024)
    if [ "$low" -gt 80 ]; then
      warn "rootless Docker cannot publish ports 80/443 (unprivileged ports start at $low). Allow it with:"
      warn "  echo 'net.ipv4.ip_unprivileged_port_start=80' | sudo tee /etc/sysctl.d/90-allod.conf && sudo sysctl --system"
    fi ;;
  esac
fi

host=$(hostname -f 2>/dev/null || hostname)
cat >&2 <<EOF

Allod $VERSION is configured in $DIR ($chain_mode chain source).
  Settings: $ENV (mode 0600; keep it private, it holds RPC passwords)

Start the stack:
  cd $DIR
  docker compose pull
  docker compose up -d

Check it:
  docker compose ps
  docker compose logs -f allod ldk-server

The containers restart on their own after a reboot (restart: unless-stopped).
Stop with: docker compose down   (keeps all data in Docker volumes)
EOF
if [ "$ROOTLESS" = 1 ]; then
  cat >&2 <<EOF
With rootless Docker, the daemon must start at boot without a login:
  systemctl --user enable docker && sudo loginctl enable-linger $ME
EOF
fi

case $profiles in *,bundled,*)
  cat >&2 <<EOF

Bitcoin Core downloads the blockchain first (hours to days). You can finish setup in
the web UI meanwhile; wallet screens unlock once the node is synced.
EOF
esac

say ""
say "Open the web UI to finish setup (owner sign-in and recovery phrase):"
if [ -n "$domain" ]; then
  say "  docker compose exec allod allod setup-link --base-url https://$domain"
  say "  (once DNS points here; Caddy fetches the certificate on first start)"
fi
say "  SSH tunnel: ssh -L 8029:127.0.0.1:8029 $ME@$host"
say "  then: docker compose exec allod allod setup-link --base-url http://127.0.0.1:8029"
case $profiles in *,tor,*)
  say "  Tor: after start, the onion address is in:"
  say "    docker compose exec tor cat /var/lib/tor/services/allod/hostname" ;;
esac

say ""
say "Firewall: allow SSH, Lightning P2P${domain:+ and HTTP/HTTPS}. With ufw, for example:"
u="${SUDO:+sudo }ufw allow"
say "  $u 22/tcp && $u $lnport/tcp${domain:+ && $u 80/tcp && $u 443}"
say "Docker publishes ports past ufw's rules, so also check your provider's firewall."
