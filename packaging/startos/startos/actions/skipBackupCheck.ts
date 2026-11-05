import { storeJson } from '../fileModels/store.json'
import { i18n } from '../i18n'
import { sdk } from '../sdk'

// Testing only (ALLOD_SKIP_BACKUP_CHECK, SPEC §3.1): lets the setup wizard skip
// re-entering recovery phrase words. The unverified-backup banner stays until
// the owner checks the backup in Allod.
export const skipBackupCheck = sdk.Action.withoutInput(
  'skip-backup-check',
  async ({ effects }) => {
    const on = !!(await storeJson.read((s) => s.skipBackupCheck).const(effects))
    return {
      name: on
        ? i18n('Require the recovery phrase check')
        : i18n('Allow skipping the recovery phrase check'),
      description: i18n(
        'Testing only: shows a Skip check button in the setup wizard. Allod restarts.',
      ),
      warning: on
        ? null
        : i18n(
            'Only for test nodes. Without a checked backup you may lose your on-chain funds.',
          ),
      allowedStatuses: 'any',
      group: i18n('Testing'),
      visibility: 'enabled',
    }
  },
  async ({ effects }) => {
    const on = !!(await storeJson.read((s) => s.skipBackupCheck).once())
    await storeJson.merge(effects, { skipBackupCheck: !on })
    return null
  },
)
