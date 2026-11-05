import { FileHelper, utils } from '@start9labs/start-sdk'
import { manifest as bitcoinManifest } from 'bitcoin-core-startos/startos/manifest'
import { storeJson } from './fileModels/store.json'
import { i18n } from './i18n'
import { sdk } from './sdk'
import {
  bitcoindMnt,
  bitcoindRpcAddress,
  bundledRoRpcUser,
  bundledRpcAddress,
  bundledRpcUser,
  dirs,
  gossipPort,
  grpcPort,
  lsps2Lsp,
  network,
  peerPort,
  uiPort,
} from './utils'

// Same service definitions as deploy/compose.yaml (SPEC §2.3.6), mapped onto
// StartOS. All daemons share the package's network namespace, so they reach
// each other on localhost, and StartOS's own interfaces replace the published
// ports and Tor service.
//
// Chain source, picked once at the first start (like the Umbrel app):
// - The Bitcoin Core service is installed: use it (cookie auth through its RPC
//   proxy). The bundled bitcoind idles as a standby; Allod offers to start it
//   when Bitcoin Core stops answering, e.g. after it was uninstalled.
// - Otherwise: run the bundled pruned bitcoind.
// Installing or removing Bitcoin Core later never switches the source; the
// owner can change it in Allod's Settings > Bitcoin > Chain source.
const vol = (subpath: string, mountpoint: string) => ({
  volumeId: 'main' as const,
  subpath,
  mountpoint,
  readonly: false,
})

// Placeholder while the Bitcoin Core service is gone: ldk-server keeps waiting
// for it and Allod reports the outage (and the standby node) after 3 minutes.
const unavailableRpc = {
  address: 'bitcoind-unavailable.invalid:8332',
  user: '-',
  pass: '-',
}

const randomPass = () =>
  utils.getDefaultString({ charset: 'a-z,A-Z,0-9', len: 32 })

export const main = sdk.setupMain(async ({ effects }) => {
  // The critical "Confirm restore" task normally blocks the start already.
  if (await storeJson.read((s) => s.restorePending).const(effects)) {
    throw new Error(
      i18n('Run the Confirm restore action before starting Allod'),
    )
  }

  // First start: pick the chain source and the bundled node's RPC passwords.
  const saved = await storeJson.read().once()
  const chainSource =
    saved?.chainSource ??
    ((await effects.getInstalledPackages()).includes('bitcoind')
      ? 'startos-bitcoind'
      : 'bundled')
  const bundledRpcPass = saved?.bundledRpcPass ?? randomPass()
  const bundledRoRpcPass = saved?.bundledRoRpcPass ?? randomPass()
  if (
    saved?.chainSource !== chainSource ||
    saved?.bundledRpcPass !== bundledRpcPass ||
    saved?.bundledRoRpcPass !== bundledRoRpcPass
  ) {
    await storeJson.merge(effects, {
      chainSource,
      bundledRpcPass,
      bundledRoRpcPass,
    })
    console.info(`allod: chain source ${chainSource}`)
  }
  const external = chainSource === 'startos-bitcoind'
  const skipBackupCheck = !!(await storeJson
    .read((s) => s.skipBackupCheck)
    .const(effects))

  let ldkMounts = sdk.Mounts.of()
    .mountVolume(vol(dirs.ldk, '/data'))
    .mountVolume(vol(dirs.handoff, '/handoff'))

  // Full-access RPC (ldk-server, gossip) and read RPC (Allod's sync progress).
  let rpc = {
    address: bundledRpcAddress,
    user: bundledRpcUser,
    pass: bundledRpcPass,
  }
  let roRpc = { ...rpc, user: bundledRoRpcUser, pass: bundledRoRpcPass }

  // Re-runs main when the Bitcoin Core service is installed or removed.
  const extAddress = external ? await bitcoindRpcAddress(effects) : null
  if (external && extAddress) {
    ldkMounts = ldkMounts.mountDependency<typeof bitcoinManifest>({
      dependencyId: 'bitcoind',
      volumeId: 'main',
      subpath: null,
      mountpoint: bitcoindMnt,
      readonly: true,
    })
  }

  const ldkSub = sdk.SubContainer.of(
    effects,
    { imageId: 'ldk-server' },
    ldkMounts,
    'ldk-server',
  )

  if (external) {
    rpc = unavailableRpc
    if (extAddress) {
      // bitcoind writes a new cookie on every start: re-run main when it
      // changes, never on the gap while bitcoind is down.
      const cookie = await FileHelper.string(
        `${await ldkSub.rootfs}${bitcoindMnt}/.cookie`,
      )
        .read(
          (c) => c.trim(),
          (prev, next) => next === null || prev === next,
        )
        .const(effects)
      const sep = cookie?.indexOf(':') ?? -1
      if (cookie && sep > 0) {
        rpc = {
          address: extAddress,
          user: cookie.slice(0, sep),
          pass: cookie.slice(sep + 1),
        }
      } else {
        console.warn(
          'allod: Bitcoin Core has no RPC cookie yet; waiting for it',
        )
        rpc = { ...unavailableRpc, address: extAddress }
      }
    }
    // The Bitcoin Core service has one RPC user; Allod's reads use it too.
    roRpc = rpc
  }

  const bitcoindSub = sdk.SubContainer.of(
    effects,
    { imageId: 'bitcoind' },
    sdk.Mounts.of()
      .mountVolume(vol(dirs.bitcoind, '/data'))
      .mountVolume(vol(dirs.control, '/control')),
    'bitcoind',
  )
  const gossipSub = sdk.SubContainer.of(
    effects,
    { imageId: 'gossip' },
    sdk.Mounts.of().mountVolume(vol(dirs.gossip, '/data')),
    'gossip',
  )
  const allodSub = sdk.SubContainer.of(
    effects,
    { imageId: 'allod' },
    sdk.Mounts.of()
      .mountVolume(vol(dirs.allod, '/data'))
      .mountVolume(vol(dirs.handoff, '/handoff'))
      .mountVolume(vol(dirs.control, '/control')),
    'allod',
  )
  // StartOS mounts volumes root-owned; every image runs as uid 1000.
  const volSub = sdk.SubContainer.of(
    effects,
    { imageId: 'ldk-server' },
    sdk.Mounts.of().mountVolume({
      volumeId: 'main',
      subpath: null,
      mountpoint: '/vol',
      readonly: false,
    }),
    'chown',
  )
  const volDirs = Object.values(dirs).map((d) => `/vol/${d}`)

  const chainMode = external ? 'external-bitcoind' : 'bundled'
  const standbyFile = '/control/bitcoind-enabled'

  const ldkEnv = {
    LDK_NETWORK: network,
    LDK_GRPC_ADDRESS: `127.0.0.1:${grpcPort}`,
  }

  return sdk.Daemons.of(effects)
    .addOneshot('chown', {
      subcontainer: volSub,
      exec: {
        command: [
          'sh',
          '-c',
          'mkdir -p "$@" && chown -R 1000:1000 "$@"',
          'sh',
          ...volDirs,
        ],
        user: 'root',
      },
      requires: [],
    })
    .addDaemon('bitcoind', {
      subcontainer: bitcoindSub,
      exec: {
        // Standby: wait for Allod's start file (allod-core standby_bitcoind::ENABLED)
        // before running the image's own entrypoint.
        command: [
          '/usr/bin/tini',
          '--',
          'bash',
          '-c',
          `if [ "$ALLOD_STANDBY" = 1 ] && [ ! -e ${standbyFile} ]; then
  echo "[standby] idle until Allod starts this node (Settings > Bitcoin)"
  while [ ! -e ${standbyFile} ]; do sleep 5; done
  echo "[standby] started by Allod"
fi
exec /usr/local/bin/bitcoind-entrypoint.sh`,
        ],
        runAsInit: true,
        env: {
          ALLOD_STANDBY: external ? '1' : '0',
          BITCOIN_NETWORK: network,
          // Sized to the free disk, at most 50 GB (images/bitcoind/prune-size.sh).
          BITCOIN_PRUNE: 'auto',
          BITCOIN_DBCACHE: '450',
          BITCOIN_SYNC_TUNING: 'auto',
          BITCOIN_RPC_PORT: '8332',
          BITCOIN_RPC_USER: bundledRpcUser,
          BITCOIN_RPC_PASS: bundledRpcPass,
          BITCOIN_RO_RPC_USER: bundledRoRpcUser,
          BITCOIN_RO_RPC_PASS: bundledRoRpcPass,
          BITCOIN_REST: '1',
          BITCOIN_TXINDEX: '0',
          BITCOIN_BLOCKFILTERINDEX: '1',
          BITCOIN_DISABLE_WALLET: '1',
        },
      },
      ready: {
        display: i18n('Built-in Bitcoin node'),
        fn: async () => {
          if (external) {
            const idle = await bitcoindSub.exec([
              'test',
              '!',
              '-e',
              standbyFile,
            ])
            if (idle.exitCode === 0) {
              // Ready, so the daemons that need it start while it idles.
              return {
                result: 'success',
                message: i18n(
                  'Standby: Allod offers to start it if Bitcoin Core goes away',
                ),
              }
            }
          }
          const res = await bitcoindSub.exec([
            'bitcoin-cli',
            '-conf=/data/bitcoin.conf',
            '-datadir=/data',
            'uptime',
          ])
          return res.exitCode === 0
            ? {
                result: 'success',
                message: i18n('The built-in Bitcoin node is running'),
              }
            : {
                result: 'starting',
                message: i18n('The built-in Bitcoin node is starting'),
              }
        },
      },
      requires: ['chown'],
    })
    .addDaemon('gossip', {
      subcontainer: gossipSub,
      exec: {
        command: sdk.useEntrypoint(),
        runAsInit: true,
        env: {
          ALLOD_GOSSIP_NETWORK: network,
          ALLOD_GOSSIP_TARGET_PEERS: '6',
          ALLOD_GOSSIP_VERIFY: 'auto',
          ALLOD_GOSSIP_BITCOIND_URL: `http://${rpc.address}`,
          BITCOIND_RPC_USER: rpc.user,
          BITCOIND_RPC_PASS: rpc.pass,
          ALLOD_GOSSIP_SCAN_INTERVAL_SECS: '1800',
          RUST_LOG: 'info',
        },
      },
      ready: {
        display: i18n('Network graph'),
        fn: () =>
          sdk.healthCheck.checkPortListening(effects, gossipPort, {
            successMessage: i18n(
              'The Lightning network graph service is running',
            ),
            errorMessage: i18n(
              'The Lightning network graph service is not running',
            ),
          }),
      },
      requires: ['bitcoind'],
    })
    .addDaemon('ldk-server', {
      subcontainer: ldkSub,
      exec: {
        command: sdk.useEntrypoint(),
        runAsInit: true,
        env: {
          ...ldkEnv,
          LDK_CHAIN: chainMode,
          LDK_BITCOIND_RPC_ADDRESS: rpc.address,
          LDK_BITCOIND_RPC_USER: rpc.user,
          LDK_BITCOIND_RPC_PASS: rpc.pass,
          LDK_ALIAS: 'allod',
          LDK_LISTEN: `0.0.0.0:${peerPort}`,
          LDK_ANCHOR_PER_CHANNEL_RESERVE_SATS: '0',
          // Allod reaches gRPC on localhost, so the TLS cert must name it.
          LDK_TLS_HOSTS: 'localhost',
          LDK_GOSSIP: 'rgs',
          LDK_RGS_URL: `http://127.0.0.1:${gossipPort}/snapshot`,
          LDK_RGS_WAIT_SECS: '600',
          LDK_SERVER_PATHFINDING_SCORES_SOURCE_URL: '',
          LDK_SEED_HANDOFF: 'true',
          LDK_LOG_LEVEL: 'Info',
          LDK_WAIT_FOR_FEES: 'true',
          LDK_LSPS2_LSP: lsps2Lsp,
        },
      },
      ready: {
        display: i18n('Lightning node'),
        fn: async () => {
          const res = await ldkSub.exec(['/usr/local/bin/ldk-healthcheck.sh'], {
            env: ldkEnv,
          })
          return res.exitCode === 0
            ? {
                result: 'success',
                message: i18n('The Lightning node is running'),
              }
            : {
                result: 'loading',
                message: i18n(
                  'Waiting for setup in the web UI, the Bitcoin sync, or the network graph',
                ),
              }
        },
      },
      requires: ['gossip'],
    })
    .addDaemon('allod', {
      subcontainer: allodSub,
      exec: {
        command: sdk.useEntrypoint(),
        env: {
          ALLOD_MODE: 'managed',
          ALLOD_PLATFORM: 'startos',
          ALLOD_NETWORK: network,
          ALLOD_CHAIN: chainMode,
          ALLOD_LISTEN: `0.0.0.0:${uiPort}`,
          ALLOD_LDK_SERVER_ADDRESS: `localhost:${grpcPort}`,
          ALLOD_BITCOIND_RPC_URL: `http://${roRpc.address}`,
          ALLOD_BITCOIND_RPC_USER: roRpc.user,
          ALLOD_BITCOIND_RPC_PASS: roRpc.pass,
          // The bundled bitcoind as a standby; empty when it is the chain source.
          ALLOD_STANDBY_BITCOIND_RPC_ADDRESS: external ? bundledRpcAddress : '',
          ALLOD_STANDBY_BITCOIND_RPC_USER: external ? bundledRpcUser : '',
          ALLOD_STANDBY_BITCOIND_RPC_PASS: external ? bundledRpcPass : '',
          ALLOD_GOSSIP_SIDECAR_URL: `http://127.0.0.1:${gossipPort}/status`,
          ALLOD_LSPS2_LSP: lsps2Lsp,
          ALLOD_LOG: 'info',
          ALLOD_SKIP_BACKUP_CHECK: skipBackupCheck ? 'true' : '',
        },
      },
      ready: {
        display: i18n('Web Interface'),
        fn: () =>
          sdk.healthCheck.checkPortListening(effects, uiPort, {
            successMessage: i18n('The web interface is ready'),
            errorMessage: i18n('The web interface is not ready'),
          }),
      },
      requires: ['chown'],
    })
    .addHealthCheck('api', {
      ready: {
        display: i18n('Allod API'),
        fn: async () => {
          try {
            const res = await fetch(`http://127.0.0.1:${uiPort}/api/health`)
            const body = (await res.json()) as { status?: string }
            if (body.status === 'ok') {
              return {
                result: 'success',
                message: i18n('Allod and the Lightning node answer'),
              }
            }
            return {
              result: 'loading',
              message: i18n(
                'Allod runs; the Lightning node is not answering yet',
              ),
            }
          } catch {
            return {
              result: 'failure',
              message: i18n('Allod API is not answering'),
            }
          }
        },
      },
      requires: ['allod'],
    })
})
