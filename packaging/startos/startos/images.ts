// Images: one tag for all three, from the Allod release pipeline (SPEC §2.3.8).
// Dated nightlies from manual runs are amd64 only; a release or scheduled
// nightly tag is multi-arch, so add 'aarch64' (and `arm` in the Makefile) then.
export const imageTag = 'nightly-20261105-2345'
export const imageArch = ['x86_64'] as ['x86_64']
