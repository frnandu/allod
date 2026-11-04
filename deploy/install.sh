#!/bin/sh
# Allod VPS installer (SPEC §2.3.5).
#
# Sets up the Allod Docker Compose stack in /opt/allod from a release bundle and prints
# how to start it. It never starts the stack itself and never touches an existing .env.
#
# Recommended: download, read, verify, run:
#   curl -fsSLO https://github.com/frnandu/allod/releases/latest/download/install.sh
#   curl -fsSLO https://github.com/frnandu/allod/releases/latest/download/SHA256SUMS
#   sha256sum -c --ignore-missing SHA256SUMS
#   less install.sh
#   sudo sh install.sh
# Shortcut:
#   curl -fsSL https://github.com/frnandu/allod/releases/latest/download/install.sh | sudo sh
#
# Options:
#   --version VER       install release VER (e.g. 0.1.1) instead of the latest
#   --dir DIR           install directory (default /opt/allod)
#   --bundle FILE       use a local allod-stack-<ver>.tar.gz instead of downloading;
#                       verified against SHA256SUMS next to it when present
#   --non-interactive   ask nothing; settings come from --env-file and defaults
#   --env-file FILE     KEY=VALUE settings applied over .env.example
#   -h, --help          show this help
set -eu

REPO_URL=${ALLOD_REPO_URL:-https://github.com/frnandu/allod}
DIR=/opt/allod
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
  --dir DIR           install directory (default /opt/allod)
  --bundle FILE       use a local allod-stack-<ver>.tar.gz instead of downloading
  --non-interactive   ask nothing; settings come from --env-file and defaults
  --env-file FILE     KEY=VALUE settings applied over .env.example
EOF2
}

while [ $# -gt 0 ]; do
  case $1 in
    --version) VERSION=${2:?--version needs a value}; VERSION=${VERSION#v}; shift 2 ;;
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

[ "$(id -u)" = 0 ] || die "run as root (sudo sh install.sh)"

OS_ID='' OS_VER='' OS_CODENAME=''
if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  OS_ID=${ID:-} OS_VER=${VERSION_ID:-} OS_CODENAME=${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}
fi
os_ok=0
case $OS_ID in
  debian) [ "${OS_VER%%.*}" -ge 12 ] 2>/dev/null && os_ok=1 ;;
  ubuntu) [ "${OS_VER%%.*}" -ge 22 ] 2>/dev/null && os_ok=1 ;;
esac
[ "$os_ok" = 1 ] || warn "supported hosts are Debian 12+ and Ubuntu 22.04+ (found ${OS_ID:-unknown} ${OS_VER}); continuing"

case $(uname -m) in
  x86_64|amd64|aarch64|arm64) ;;
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
  say "Installing Docker Engine from download.docker.com..."
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -q
  apt-get install -y -q ca-certificates curl
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL "https://download.docker.com/linux/$OS_ID/gpg" -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/$OS_ID $OS_CODENAME stable" \
    >/etc/apt/sources.list.d/docker.list
  apt-get update -q
  apt-get install -y -q docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
  systemctl enable --now docker
}

if ! have docker || ! docker compose version >/dev/null 2>&1; then
  say "Docker Engine with the compose plugin is not installed."
  if confirm "Install it from Docker's apt repository now?" y; then install_docker
  else die "Allod needs Docker Engine and the compose plugin"; fi
fi
docker info >/dev/null 2>&1 || die "the Docker daemon is not running (systemctl start docker)"

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
  if [ -n "$VERSION" ]; then base="$REPO_URL/releases/download/v$VERSION"; else base="$REPO_URL/releases/latest/download"; fi
  say "Downloading release from $base"
  fetch "$base/SHA256SUMS" "$WORK/SHA256SUMS" || die "cannot download $base/SHA256SUMS"
  name=$(awk '{ sub(/^\*/, "", $2) } $2 ~ /^allod-stack-.*\.tar\.gz$/ { print $2; exit }' "$WORK/SHA256SUMS")
  [ -n "$name" ] || die "SHA256SUMS lists no allod-stack bundle"
  fetch "$base/$name" "$WORK/$name" || die "cannot download $base/$name"
  (cd "$WORK" && grep " \*\{0,1\}$name\$" SHA256SUMS | sha256sum -c -) >&2 || die "checksum mismatch for $name"
fi
VERSION=${name#allod-stack-}; VERSION=${VERSION%.tar.gz}
tar -C "$WORK" -xzf "$WORK/$name"
SRC="$WORK/allod-stack-$VERSION"
{ [ -f "$SRC/compose.yaml" ] && [ -f "$SRC/.env.example" ]; } || die "$name is not an Allod stack bundle"

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
chain=$(get_env "$ENV" ALLOD_CHAIN)
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

host=$(hostname -f 2>/dev/null || hostname)
cat >&2 <<EOF

Allod $VERSION is configured in $DIR ($chain chain source).
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

case $profiles in *,bundled,*)
  cat >&2 <<EOF

Bitcoin Core downloads the blockchain first (hours to days). You can finish setup in
the web UI meanwhile; wallet screens unlock once the node is synced.
EOF
esac

say ""
say "Open the web UI to finish setup (owner sign-in and recovery phrase):"
if [ -n "$domain" ]; then
  say "  https://$domain   (once DNS points here; Caddy fetches the certificate on first start)"
fi
say "  SSH tunnel: ssh -L 8029:127.0.0.1:8029 root@$host   then http://127.0.0.1:8029"
case $profiles in *,tor,*)
  say "  Tor: after start, the onion address is in:"
  say "    docker compose exec tor cat /var/lib/tor/services/allod/hostname" ;;
esac

say ""
say "Firewall: allow SSH, Lightning P2P${domain:+ and HTTP/HTTPS}. With ufw, for example:"
say "  ufw allow 22/tcp && ufw allow $lnport/tcp${domain:+ && ufw allow 80/tcp && ufw allow 443}"
say "Docker publishes ports past ufw's rules, so also check your provider's firewall."
