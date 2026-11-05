import { i18n } from '../i18n'
import { sdk } from '../sdk'
import { peerHostId, uiHostId } from '../interfaces'
import { peerPort, uiPort } from '../utils'

type Single = {
  type: 'single'
  name: string
  description: null
  value: string
  masked: false
  copyable: true
  qr: boolean
  launchable: false
}
const single = (name: string, value: string, qr = false): Single => ({
  type: 'single',
  name,
  description: null,
  value,
  masked: false,
  copyable: true,
  qr,
  launchable: false,
})

export const connectionInfo = sdk.Action.withoutInput(
  'connection-info',
  async () => ({
    name: i18n('Show onion and connection info'),
    description: i18n(
      'Node ID, Lightning peer URIs and the web UI addresses (LAN and Tor)',
    ),
    warning: null,
    allowedStatuses: 'only-running',
    group: null,
    visibility: 'enabled',
  }),
  async ({ effects }) => {
    let pubkey: string | null = null
    try {
      const res = await fetch(`http://127.0.0.1:${uiPort}/api/health`)
      const body = (await res.json()) as { node?: { pubkey?: string } }
      pubkey = body.node?.pubkey ?? null
    } catch {
      pubkey = null
    }

    const peerHost = await sdk.host.getOwn(effects, peerHostId).once()
    const peerAddrs =
      peerHost?.bindings[peerPort]?.interfaces[
        'peer'
      ]?.addressInfo.nonLocal.format() ?? []
    const uiHost = await sdk.host.getOwn(effects, uiHostId).once()
    const uiAddrs =
      uiHost?.bindings[uiPort]?.interfaces[
        'ui'
      ]?.addressInfo.nonLocal.format() ?? []

    const strip = (u: string) =>
      u.replace(/^[a-z]+:\/\//, '').replace(/\/$/, '')
    const value: Single[] = []
    if (pubkey) {
      value.push(single(i18n('Node ID'), pubkey, true))
      for (const a of peerAddrs)
        value.push(single(i18n('Peer URI'), `${pubkey}@${strip(a)}`, true))
    }
    for (const a of uiAddrs) value.push(single(i18n('Web UI'), a))

    return {
      version: '1',
      title: i18n('Connection info'),
      message: pubkey
        ? null
        : i18n(
            'The Lightning node is not running yet. Finish setup in the web UI.',
          ),
      result: { type: 'group', value },
    }
  },
)
