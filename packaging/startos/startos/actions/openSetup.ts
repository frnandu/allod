import { i18n } from '../i18n'
import { sdk } from '../sdk'
import { uiHostId } from '../interfaces'
import { dirs, uiPort } from '../utils'

type SetupLink = {
  type: 'single'
  name: string
  description: null
  value: string
  masked: false
  copyable: true
  qr: true
  launchable: true
}

export const openSetup = sdk.Action.withoutInput(
  'open-setup',
  async () => ({
    name: i18n('Open setup'),
    description: i18n('Create clickable setup links for this Allod instance'),
    warning: null,
    allowedStatuses: 'only-running',
    group: null,
    visibility: 'enabled',
  }),
  async ({ effects }) => {
    const info = await fetch(`http://127.0.0.1:${uiPort}/api/info`)
    const body = (await info.json()) as { setup_completed?: boolean }
    if (body.setup_completed) {
      return {
        version: '1',
        title: i18n('Setup complete'),
        message: i18n('Open Allod from its Web UI interface.'),
        result: null,
      }
    }

    const host = await sdk.host.getOwn(effects, uiHostId).once()
    const addresses =
      host?.bindings[uiPort]?.interfaces['ui']?.addressInfo.nonLocal.format() ??
      []
    const links: SetupLink[] = []

    for (const address of addresses) {
      const res = await sdk.SubContainer.withTemp(
        effects,
        { imageId: 'allod' },
        sdk.Mounts.of().mountVolume({
          volumeId: 'main',
          subpath: dirs.allod,
          mountpoint: '/data',
          readonly: false,
        }),
        'setup-link',
        (sub) =>
          sub.exec(
            [
              '/usr/local/bin/allod',
              'setup-link',
              '--base-url',
              address,
              '--data-dir',
              '/data',
            ],
            { user: '1000' },
          ),
      )
      if (res.exitCode !== 0) throw new Error(String(res.stderr || res.stdout))
      const link = String(res.stdout)
        .split(/\s+/)
        .find((part) => part.includes('/#setup='))
      if (!link) throw new Error('allod setup-link returned no URL')
      links.push({
        type: 'single',
        name: i18n('Setup link'),
        description: null,
        value: link,
        masked: false,
        copyable: true,
        qr: true,
        launchable: true,
      })
    }

    if (!links.length)
      throw new Error('StartOS exposed no Allod Web UI address')
    return {
      version: '1',
      title: i18n('Open setup'),
      message: i18n(
        'Click a setup link. Allod removes its secret from the address bar immediately.',
      ),
      result: { type: 'group', value: links },
    }
  },
)
