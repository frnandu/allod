import { storeJson } from '../fileModels/store.json'
import { i18n } from '../i18n'
import { sdk } from '../sdk'

// SPEC §2.3.7: a restored node may hold old channel state. Broadcasting it gets
// the channel funds penalized, so startup waits for this confirmation.
export const confirmRestore = sdk.Action.withoutInput(
  'confirm-restore',
  async ({ effects }) => ({
    name: i18n('Confirm restore'),
    description: i18n(
      'Confirm that the node this backup came from is stopped for good, so Allod can start.',
    ),
    warning: i18n(
      'Restoring old Lightning channel state can lose all channel funds. Only continue if the original node is permanently stopped and this backup is its latest state. If in doubt, close channels from the original node first.',
    ),
    allowedStatuses: 'only-stopped',
    group: null,
    visibility: (await storeJson.read((s) => s.restorePending).const(effects))
      ? 'enabled'
      : 'hidden',
  }),
  async ({ effects }) => {
    await storeJson.merge(effects, { restorePending: false })
    return null
  },
)
