# Allod chain source on umbrelOS (SPEC §2.3.6).
#
# Allod declares no app dependency, so umbrelOS does not source the Bitcoin Node app's
# exports for it. Instead this file looks for that app itself:
#   umbrel-bitcoin  the Bitcoin Node app is installed: use its RPC (pruned or full)
#   bundled         otherwise: run Allod's own pruned bitcoind (compose profile `bundled`)
# The first start saves the choice and the network to data/umbrel-chain.env
# (hooks/pre-start); later starts reuse it, so installing or removing the Bitcoin Node
# app never switches a running node to another chain source. The owner can still point
# ldk-server elsewhere in Allod's Settings > Bitcoin > Chain source.
#
# Sourced by umbrelOS's app script under `set -euo pipefail`: no exit, no writes.

allod_data_root="${EXPORTS_APP_DATA_DIR:-${EXPORTS_APP_DIR}/data}"
allod_chain_file="${allod_data_root}/umbrel-chain.env"
allod_btc_app_dir="${UMBREL_ROOT}/app-data/bitcoin"

allod_chain=""
allod_network=""
if [[ -f "${allod_chain_file}" ]]; then
	allod_chain="$(sed -n 's/^ALLOD_UMBREL_CHAIN=//p' "${allod_chain_file}" | head -n 1)" || true
	allod_network="$(sed -n 's/^ALLOD_UMBREL_NETWORK=//p' "${allod_chain_file}" | head -n 1)" || true
fi

case "${allod_chain}" in
	umbrel-bitcoin | bundled) ;;
	*)
		allod_chain="bundled"
		# The app's .env holds the RPC credentials; it exists once the app was started.
		if [[ -f "${allod_btc_app_dir}/umbrel-app.yml" ]] && [[ -f "${allod_btc_app_dir}/.env" ]]; then
			# Leftover files of an uninstalled app don't count. Capture first: under
			# pipefail, `grep -q` closing the pipe early would fail the whole test.
			allod_installed_apps="bitcoin"
			if declare -F list_installed_apps > /dev/null; then
				allod_installed_apps="$(list_installed_apps 2> /dev/null)" || true
			fi
			if grep -qx "bitcoin" <<< "${allod_installed_apps}"; then
				allod_chain="umbrel-bitcoin"
			fi
		fi
		;;
esac

if [[ "${allod_chain}" == "umbrel-bitcoin" ]]; then
	# RPC credentials as the Bitcoin Node app exports them (APP_BITCOIN_RPC_USER/PASS).
	APP_BITCOIN_RPC_USER=""
	APP_BITCOIN_RPC_PASS=""
	if [[ -f "${allod_btc_app_dir}/.env" ]]; then
		# shellcheck disable=SC1091
		. "${allod_btc_app_dir}/.env" || true
	fi
	if [[ -z "${allod_network}" ]]; then
		# Same rule as the Bitcoin Node app's exports.sh: settings.json `chain`, else mainnet.
		allod_btc_chain="$(jq -r '.chain // empty' "${allod_btc_app_dir}/data/app/settings.json" 2> /dev/null)" || true
		case "${allod_btc_chain}" in
			test) allod_network="testnet" ;;
			testnet4 | signet | regtest) allod_network="${allod_btc_chain}" ;;
			*) allod_network="bitcoin" ;;
		esac
	fi
	# Stable address of the Bitcoin Node app on umbrel_main_network (its exports.sh).
	export APP_ALLOD_BITCOIND_RPC_ADDRESS="10.21.21.8:8332"
	export APP_ALLOD_BITCOIND_RPC_USER="${APP_BITCOIN_RPC_USER}"
	export APP_ALLOD_BITCOIND_RPC_PASS="${APP_BITCOIN_RPC_PASS}"
	# The app has one RPC user; Allod's sync-progress reads use it too.
	export APP_ALLOD_BITCOIND_RO_RPC_USER="${APP_BITCOIN_RPC_USER}"
	export APP_ALLOD_BITCOIND_RO_RPC_PASS="${APP_BITCOIN_RPC_PASS}"
	export APP_ALLOD_LDK_CHAIN="external-bitcoind"
	export COMPOSE_PROFILES=""
else
	allod_network="${allod_network:-bitcoin}"
	export APP_ALLOD_BITCOIND_RPC_ADDRESS="allod_bitcoind_1:8332"
	# derive_entropy and app_entropy_identifier come from umbrelOS's app script.
	# shellcheck disable=SC2154
	APP_ALLOD_BITCOIND_RPC_PASS="$(derive_entropy "${app_entropy_identifier}-bitcoind-rpc")"
	APP_ALLOD_BITCOIND_RO_RPC_PASS="$(derive_entropy "${app_entropy_identifier}-bitcoind-ro-rpc")"
	export APP_ALLOD_BITCOIND_RPC_USER="ldk"
	export APP_ALLOD_BITCOIND_RPC_PASS
	export APP_ALLOD_BITCOIND_RO_RPC_USER="allod-ro"
	export APP_ALLOD_BITCOIND_RO_RPC_PASS
	export APP_ALLOD_LDK_CHAIN="bundled"
	export COMPOSE_PROFILES="bundled"
fi

case "${allod_network}" in
	bitcoin | testnet | testnet4 | signet | regtest) ;;
	*) allod_network="bitcoin" ;;
esac

export APP_ALLOD_CHAIN_SOURCE="${allod_chain}"
export APP_ALLOD_NETWORK="${allod_network}"

# Mainnet LSPS2 LSP for just-in-time receive channels; none on test networks.
if [[ "${allod_network}" == "bitcoin" ]]; then
	export APP_ALLOD_LSPS2_LSP="034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735"
else
	export APP_ALLOD_LSPS2_LSP=""
fi
