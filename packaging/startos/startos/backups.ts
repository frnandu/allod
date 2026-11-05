import { sdk } from './sdk'
import { dirs } from './utils'

// StartOS stops the service for the backup, so the SQLite files are quiescent.
// The gossip graph is re-downloaded on start. Platform snapshots are a last
// resort only: restoring old channel state can lose funds (SPEC §2.3.7), so a
// restore raises a critical task (init/restoreTask.ts).
export const { createBackup, restoreInit } = sdk.setupBackups(async () =>
  sdk.Backups.withOptions({ exclude: [dirs.gossip, dirs.bitcoind] }).addVolume(
    'main',
  ),
)
