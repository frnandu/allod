import { setupManifest } from '@start9labs/start-sdk'
import { long, short } from './i18n'
import { imageArch, imageTag } from '../images'

export const manifest = setupManifest({
  id: 'allod',
  title: 'Allod',
  license: 'MIT',
  packageRepo: 'https://github.com/frnandu/allod/tree/master/packaging/startos',
  upstreamRepo: 'https://github.com/frnandu/allod',
  marketingUrl: 'https://github.com/frnandu/allod',
  donationUrl: null,
  description: { short, long },
  // One volume, one subfolder per daemon (see utils.ts `dirs`).
  volumes: ['main'],
  images: {
    allod: {
      source: { dockerTag: `ghcr.io/frnandu/allod:${imageTag}` },
      arch: imageArch,
    },
    'ldk-server': {
      source: { dockerTag: `ghcr.io/frnandu/allod-ldk-server:${imageTag}` },
      arch: imageArch,
    },
    bitcoind: {
      source: { dockerTag: `ghcr.io/frnandu/allod-bitcoind:${imageTag}` },
      arch: imageArch,
    },
    gossip: {
      source: { dockerTag: `ghcr.io/frnandu/allod-gossip:${imageTag}` },
      arch: imageArch,
    },
  },
})
