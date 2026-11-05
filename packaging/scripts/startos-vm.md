# StartOS test VM

A libvirt VM (`startos`, 4 vCPU, 8 GB RAM, 120 GB thin disk, UEFI) on the default NAT
network, installed from the StartOS 0.4.1.2 slim x86_64 ISO. Files are in
`~/workspace/startos-vm/` (disk, ISO, root CA, master password).

```sh
virsh -c qemu:///system start startos       # web UI: https://192.168.122.174 (or https://allod-test.local)
export SSL_CERT_FILE=~/workspace/startos-vm/ca-bundle.pem   # system CAs + the VM's root CA, for start-cli
start-cli auth login                         # master password: ~/workspace/startos-vm/master-password.txt
```

Dev registry: the `startos-registry` service runs in the same VM. Its Web API is the
`api-multi` host's SSL port (see `start-cli db dump`, currently `https://192.168.122.174:55653`).
The `.s9pk` it indexes is served from the host by an nginx container bound to the libvirt
bridge:

```sh
docker run -d --name allod-s9pk-dev --restart unless-stopped \
  -p 192.168.122.1:8766:80 -v "$PWD/startos/dist:/usr/share/nginx/html:ro" nginx:1.27-alpine
cp startos/allod_x86_64.s9pk startos/dist/
start-cli -r https://192.168.122.174:55653 registry package add startos/dist/allod_x86_64.s9pk \
  --url http://192.168.122.1:8766/allod_x86_64.s9pk
```

Bump `version` in `startos/startos/versions/current.ts` for every republish, or StartOS
offers no update.
