import { T } from '@start9labs/start-sdk'
import {
  rpcHostId as btcRpcHostId,
  rpcPort as btcRpcPort,
} from 'bitcoin-core-startos/startos/utils'
import { sdk } from './sdk'

export const uiPort = 8029
export const peerPort = 9735
export const grpcPort = 3536
export const gossipPort = 8090

// Bitcoin Core on StartOS is mainnet only.
export const network = 'bitcoin'

// Mainnet LSPS2 LSP for just-in-time receive channels (same as deploy/.env.example).
export const lsps2Lsp =
  '034066e29e402d9cf55af1ae1026cc5adf92eed1e0e421785442f53717ad1453b0@64.23.159.177:9735'

// Subfolders of the `main` volume. The handoff folder is shared by ldk-server
// and Allod (seed and credential handoff, SPEC §2.3.3).
export const dirs = {
  allod: 'allod',
  ldk: 'ldk',
  gossip: 'gossip',
  handoff: 'handoff',
  control: 'control',
  bitcoind: 'bitcoind',
} as const

// The bundled bitcoind's RPC (same network namespace as the other daemons).
export const bundledRpcAddress = '127.0.0.1:8332'
export const bundledRpcUser = 'ldk'
export const bundledRoRpcUser = 'allod-ro'

export const bitcoindMnt = '/mnt/bitcoin'

/**
 * `host:port` of the Bitcoin Core service's RPC on the StartOS bridge, or null
 * when that service is not installed.
 */
const bitcoindRpcAddressQuery = (effects: T.Effects) =>
  sdk.host.getBridgeAddress(effects, {
    packageId: 'bitcoind',
    hostId: btcRpcHostId,
    internalPort: btcRpcPort,
    ssl: false,
  })
export const bitcoindRpcAddress = (effects: T.Effects) =>
  bitcoindRpcAddressQuery(effects).const()
