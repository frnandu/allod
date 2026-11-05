import { i18n } from '../i18n'
import { sdk } from '../sdk'
import { dirs } from '../utils'
const { InputSpec, Value } = sdk

const inputSpec = InputSpec.of({
  pubkey: Value.text({
    name: i18n('Nostr public key'),
    description: i18n('npub or hex public key that may sign in to the web UI'),
    required: true,
    default: null,
    placeholder: 'npub1…',
    patterns: [
      {
        regex: '^(npub1[02-9ac-hj-np-z]{58}|[0-9a-fA-F]{64})$',
        description: i18n('Must be an npub or a 64-character hex key'),
      },
    ],
  }),
})

// The recovery path for a lost Nostr key (SPEC §11.1): runs `allod owner add`.
export const addOwner = sdk.Action.withInput(
  'add-owner',
  async () => ({
    name: i18n('Add owner key'),
    description: i18n(
      'Allow another Nostr key to sign in to Allod. Use it if you lost access to your signer.',
    ),
    warning: i18n(
      'This key gets full control of the wallet and the Lightning node.',
    ),
    allowedStatuses: 'any',
    group: null,
    visibility: 'enabled',
  }),
  inputSpec,
  async () => {},
  async ({ effects, input }) => {
    const res = await sdk.SubContainer.withTemp(
      effects,
      { imageId: 'allod' },
      sdk.Mounts.of().mountVolume({
        volumeId: 'main',
        subpath: dirs.allod,
        mountpoint: '/data',
        readonly: false,
      }),
      'add-owner',
      (sub) =>
        sub.exec(
          [
            '/usr/local/bin/allod',
            'owner',
            'add',
            input.pubkey,
            '--label',
            'startos',
          ],
          {
            user: '1000',
          },
        ),
    )
    if (res.exitCode !== 0) {
      throw new Error(String(res.stderr || res.stdout))
    }
    return {
      version: '1',
      title: i18n('Owner added'),
      message: String(res.stdout).trim(),
      result: null,
    }
  },
)
