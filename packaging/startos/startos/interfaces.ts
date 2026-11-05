import { i18n } from './i18n'
import { sdk } from './sdk'
import { peerPort, uiPort } from './utils'

export const uiHostId = 'ui'
export const peerHostId = 'peer'

export const setInterfaces = sdk.setupInterfaces(async ({ effects }) => {
  const uiOrigin = await sdk.MultiHost.of(effects, uiHostId).bindPort(uiPort, {
    protocol: 'http',
  })
  const ui = sdk.createInterface(effects, {
    name: i18n('Web UI'),
    id: 'ui',
    description: i18n('Allod web interface: wallet, node, and connections'),
    type: 'ui',
    schemeOverride: null,
    masked: false,
    username: null,
    path: '',
    query: {},
  })

  const peerOrigin = await sdk.MultiHost.of(effects, peerHostId).bindPort(
    peerPort,
    {
      protocol: null,
      addSsl: null,
      preferredExternalPort: peerPort,
      secure: { ssl: false },
    },
  )
  const peer = sdk.createInterface(effects, {
    name: i18n('Lightning P2P'),
    id: 'peer',
    description: i18n('Lightning peers connect to your node here'),
    type: 'p2p',
    schemeOverride: null,
    masked: false,
    username: null,
    path: '',
    query: {},
  })

  return [await uiOrigin.export([ui]), await peerOrigin.export([peer])]
})
