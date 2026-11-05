import { FileHelper, z } from '@start9labs/start-sdk'
import { sdk } from '../sdk'

const shape = z.looseObject({
  // Set when the package data came from a backup; cleared by the
  // "Confirm restore" action (stale channel state, SPEC §2.3.7).
  restorePending: z.boolean().catch(false),
  // Chain source, picked once at the first start (SPEC §2.3.6):
  //   startos-bitcoind  the Bitcoin Core service was installed; the bundled
  //                     bitcoind idles as a standby
  //   bundled           Allod's own pruned bitcoind
  // null until the first start.
  chainSource: z.enum(['startos-bitcoind', 'bundled']).nullable().catch(null),
  // RPC credentials of the bundled bitcoind (full access, and read-only for Allod).
  bundledRpcPass: z.string().nullable().catch(null),
  bundledRoRpcPass: z.string().nullable().catch(null),
  // Testing only: the setup wizard may skip the recovery phrase check
  // (ALLOD_SKIP_BACKUP_CHECK, SPEC §3.1).
  skipBackupCheck: z.boolean().catch(false),
})

export const storeJson = FileHelper.json(
  { base: sdk.volumes.main, subpath: './startos-store.json' },
  shape,
)
