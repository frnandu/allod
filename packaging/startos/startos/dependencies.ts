import { storeJson } from './fileModels/store.json'
import { depBitcoindDescription } from './manifest/i18n'
import { sdk } from './sdk'

// Optional: Allod runs its own bundled bitcoind when the Bitcoin Core service is
// not installed. It is enabled (and its volume mounted for the RPC cookie) only
// when it was the chain source picked at the first start (SPEC §2.3.6).
export const dependencies = sdk.Dependencies.of().addDependency(
  sdk.Dependency.optional('bitcoind', {
    description: depBitcoindDescription,
    metadata: {
      title: 'Bitcoin',
      icon: 'https://raw.githubusercontent.com/Start9Labs/bitcoin-core-startos/feec0b1dae42961a257948fe39b40caf8672fce1/dep-icon.svg',
    },
    // Same range LND accepts: Bitcoin Core 28–31 and Knots.
    versionRange:
      '(>=28.4:29 && <29) || (>=29.4:16 && <30) || (>=30.3:16 && <31) || >=31.1:16 || >=#knotsprerdts:29.3:29',
    kind: 'running',
    healthChecks: ['bitcoind', 'sync-progress'],
    enabled: async ({ effects }) =>
      (await storeJson.read((s) => s.chainSource).const(effects)) ===
      'startos-bitcoind',
  }),
)
