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
      source: { dockerTag: `docker.io/allodme/allod:${imageTag}` },
      arch: imageArch,
    },
    'ldk-server': {
      source: { dockerTag: `docker.io/allodme/allod-ldk-server:${imageTag}` },
      arch: imageArch,
    },
    bitcoind: {
      source: { dockerTag: `docker.io/allodme/allod-bitcoind:${imageTag}` },
      arch: imageArch,
    },
    gossip: {
      source: { dockerTag: `docker.io/allodme/allod-gossip:${imageTag}` },
      arch: imageArch,
    },
  },
})
