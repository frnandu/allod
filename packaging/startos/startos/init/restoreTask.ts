import { confirmRestore } from '../actions/confirmRestore'
import { storeJson } from '../fileModels/store.json'
import { i18n } from '../i18n'
import { sdk } from '../sdk'

// On restore, block startup behind the stale-channel-state warning (SPEC §2.3.6).
export const restoreTask = sdk.setupOnInit(async (effects, kind) => {
  if (kind === 'install') {
    await storeJson.merge(effects, { restorePending: false })
  }
  if (kind === 'restore') {
    await storeJson.merge(effects, { restorePending: true })
    await sdk.action.createOwnTask(effects, confirmRestore, 'critical', {
      reason: i18n(
        'Confirm the original node is stopped before starting a restored node',
      ),
    })
  }
})
