# Allod appliance images (proof of concept)

Bootable images that run the same `deploy/compose.yaml` stack as a VPS install
(SPEC §2.3.10). Write one to a USB stick or SD card; the stack runs from a separate data disk.

| File | For | Size |
|---|---|---|
| `allod-amd64.iso` | PCs (BIOS/UEFI), USB stick or CD | about 830 MB |
| `allod-amd64-snapshot.iso` | same, plus the mainnet UTXO snapshot | about 9.6 GB (16 GB stick) |
| `allod-rpi-arm64.img` | Raspberry Pi 4 and 5, SD card or USB | about 1 GB |
| `allod-rpi-arm64-snapshot.img` | same, plus the mainnet UTXO snapshot | about 10 GB (16 GB card) |

```sh
just iso          # build the local images, then iso/out/allod-amd64.iso
just iso-rpi      # Raspberry Pi image from the registry's arm64 images (ALLOD_RPI_VERSION,
                  # default nightly); needs qemu-user-static on the host
ALLOD_ISO_SNAPSHOT=$PWD/iso/work/snapshot/utxo-935000.dat just iso   # also the -snapshot variant
just iso-run      # boot it in QEMU with a blank 60 GB disk; UI on http://127.0.0.1:18080
just iso-run --vnc   # same, and the VM's screen in your browser (noVNC; URL is printed)
just iso-run --gui   # same, screen in a desktop window
sudo dd if=iso/out/allod-amd64.iso of=/dev/sdX bs=4M conv=fsync   # USB stick
```

Raspberry Pi: write `allod-rpi-arm64.img` with Raspberry Pi Imager ("Use custom") or `dd`.
Use a USB SSD or NVMe drive (Pi 5) as the data disk; the SD card only holds the system.

## What it is

- Debian trixie live system (`live-boot`, squashfs root, changes in RAM), Docker CE and
  the Compose plugin. PC: hybrid BIOS/UEFI ISO. Raspberry Pi: MBR image with the Pi
  firmware, Raspberry Pi kernels (`kernel8.img` for Pi 4, `kernel_2712.img` for Pi 5) and
  `rpi/config.txt` on a FAT partition next to the squashfs.
- The images of every Compose profile are baked in. Nothing is pulled at boot.
- The UI listens on port 8029 on all interfaces: `http://<ip>:8029` or `http://allod.local:8029`.
- Local display: a Plymouth splash with the Allod logo from early boot, then a kiosk
  (cage + LibreWolf, full screen on tty1) that shows a "Starting…" page and opens the
  UI as soon as Allod answers. Sign in there with a NIP-46 signer (QR code). Kiosk
  settings: `/usr/local/bin/allod-kiosk`. LibreWolf reports the time zone as UTC.
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

## Network and Wi-Fi

Ethernet works without setup (DHCP). For Wi-Fi, any of these:

1. **Before the first start:** fill in `allod-network.txt` on the stick or card
   (`WIFI_SSID`, `WIFI_PASSWORD`, `WIFI_COUNTRY`). On the Raspberry Pi image it is on the
   boot partition; on the PC image on the small `ALLODCFG` partition that appears after
   writing the image to a USB stick. The password is removed from the file once saved.
2. **Setup hotspot:** without a network for 60 s the device opens the Wi-Fi
   `Allod-Setup-XXXX`. Join it with a phone; the setup page opens by itself (or go to
   `http://10.42.0.1`). It closes once the device is on your network.
3. **Web UI:** Settings → Network (also in the kiosk), once the device is reachable.
4. **Console:** `nmtui` on tty2 (Ctrl+Alt+F2) with a keyboard attached.

Do not use Raspberry Pi Imager's "OS customisation" with these images: it edits
`cmdline.txt` for Raspberry Pi OS and would break the boot. Use `allod-network.txt`.

Saved networks and the Wi-Fi country live on the data disk. Boot with `allod.nohotspot`
to turn the setup hotspot off.

## Snapshot variants

The `-snapshot` images carry the mainnet UTXO snapshot (`utxo-935000.dat`, 9.4 GB): in
`/snapshot` on the ISO, or on the `ALLOD_SNAP` partition of the Pi image. At boot the
host serves it on the Docker bridge only (`http://allod-usb:8088/…`) and a generated
`compose.snapshot.yaml` sets `ALLOD_FASTSYNC_URL`, so the setup wizard's "Fast sync"
suggests this copy. bitcoind copies it to the data disk and checks its hash as for a
download. Keep the stick or card in the device until fast sync has loaded.

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

- The Raspberry Pi image has not been booted on hardware yet.
- Updates: boot a newer image. There is no A/B update or rollback, and no migration guard.
- Console autologin as root on tty2; no SSH. The kiosk's browser profile is in RAM,
  so the signer session is lost at reboot. A 1–2 s black gap while Firefox starts.
- Plain HTTP on the LAN. The setup hotspot is an open network (it only offers Wi-Fi setup).
- Single data disk; no RAID/LVM across several disks; no disk encryption.
- `ldk-server` still waits for bitcoind sync and the gossip graph as on any install.
