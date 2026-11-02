# Allod appliance ISO (proof of concept)

A bootable amd64 image that runs the same `deploy/compose.yaml` stack as a VPS install.
Write it to a USB stick (or boot it as a CD/VM image); the stack runs from a separate data disk.

```sh
just iso          # build the images, then iso/out/allod-amd64.iso (about 600 MB)
just iso-run      # boot it in QEMU with a blank 60 GB disk; UI on http://127.0.0.1:18080
just iso-run --vnc   # same, and the VM's screen in your browser (noVNC; URL is printed)
just iso-run --gui   # same, screen in a desktop window
sudo dd if=iso/out/allod-amd64.iso of=/dev/sdX bs=4M conv=fsync   # USB stick
```

## What it is

- Debian trixie live system (`live-boot`, squashfs root, changes in RAM), Docker CE and
  the Compose plugin. Hybrid BIOS/UEFI boot.
- The stack images (default `bundled` profile) are baked in. Nothing is pulled at boot.
- The UI listens on port 8029 on all interfaces: `http://<ip>:8029` or `http://allod.local:8029`.
- Local display: a Plymouth splash with the Allod logo from early boot, then a kiosk
  (cage + Firefox ESR, full screen on tty1) that shows a "Starting…" page and opens the
  UI as soon as Allod answers. Sign in there with a NIP-46 signer (QR code). Firefox
  settings: `/etc/firefox/policies/policies.json`.
- Ctrl+Alt+F2: root console with the UI address and stack state.
- Boot menu (hidden; hold Shift or press Esc): normal, never auto-format a disk, and
  headless (no kiosk, serial console). Add `allod.debug` to dump splash and kiosk logs
  to the serial port. Run `allod-boot-report` to see where boot time went.

## Data disk

Everything stateful lives on the data disk, mounted at `/var/lib/allod`: Docker's and
containerd's storage, and the stack directory (`stack/.env`, `ldk-data`, `allod-data`,
`bitcoind-data`, ...). At boot `allod-data-setup` does this:

1. A filesystem labelled `ALLOD_DATA` is used.
2. Otherwise the largest **blank** disk of at least 10 GB is formatted (GPT, ext4,
   `ALLOD_DATA`). Blank means no partition table and no filesystem signature, and the
   boot disk is never chosen. A disk with data on it is never touched. The second boot
   menu entry (`allod.noformat`) disables this step.
3. Otherwise the stack does not start: channel state must never live in RAM. Run
   `allod-format-disk /dev/sdX` on the console (asks for confirmation).

On the first boot with a new image the images are loaded (`docker load`) and
`docker compose up -d` runs. On later boots dockerd restarts the containers itself and
compose is skipped unless `compose.yaml`, `.env` or the image set changed.

`.env` is generated on the first boot from `.env.example` with fresh RPC passwords. Edit
`/var/lib/allod/stack/.env` and run `systemctl restart allod-stack` to change settings.

## Boot time (QEMU/KVM, 4 vCPU, measured)

| Boot | Power-on to UI answering |
|---|---|
| ISO as USB-style disk, normal boot | about 7–8 s (BIOS or UEFI) without kiosk |
| Same, with kiosk: splash / UI on screen | splash at about 3 s, UI on screen at about 9 s |
| First boot with a new image (format disk, `docker load`, `compose up`) | about 15 s, plus image load on the first boot ever |
| ISO as CD drive | about 35 s (slow emulated CD reads) |

Kernel to stack started is about 5.5 s; dockerd (restoring containers) is the largest
part, about 1.5 s. Real hardware adds its firmware time (typically 2–10 s).

## Not done (proof of concept)

- Updates: boot a newer ISO. There is no A/B update or rollback, and no migration guard.
- Console autologin as root on tty2; no SSH. The kiosk's Firefox profile is in RAM,
  so the signer session is lost at reboot. A 1–2 s black gap while Firefox starts.
- Plain HTTP on the LAN; no onboarding hotspot; wired DHCP only.
- Single data disk; no RAID/LVM across several disks; no disk encryption.
- `ldk-server` still waits for bitcoind sync and the gossip graph as on any install.
