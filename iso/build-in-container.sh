#!/bin/sh
# Runs inside debian:trixie (privileged). Called by build.sh; not meant to run on the host.
# ARCH=amd64 → ISO for PCs. ARCH=arm64 → SD/USB image for Raspberry Pi 4 and 5 (needs
# qemu-user-static with binfmt on the host to run arm64 package scripts).
# A snapshot mounted at /snapshot/utxo-*.dat adds a "-snapshot" variant.
set -eu

iso=/iso
root=/build/rootfs
tree=/build/tree
snapshot=$(ls /snapshot/*.dat 2>/dev/null | head -1 || true)

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  debootstrap squashfs-tools xorriso grub-pc-bin grub-efi-amd64-bin grub-common mtools \
  dosfstools fdisk e2fsprogs zstd ca-certificates curl gpg librsvg2-bin fonts-dejavu-core >/dev/null

mounts() {
  mount -t proc proc "$root/proc"
  mount -t sysfs sys "$root/sys"
  mount --bind /dev "$root/dev"
  mount -t devpts devpts "$root/dev/pts" 2>/dev/null || true
}
umounts() { umount -l "$root/dev/pts" "$root/proc" "$root/sys" "$root/dev" 2>/dev/null || true; }
trap umounts EXIT

# 1. Minimal Debian with every package installed. This is the slow part (on arm64 every
# package script runs under qemu-user), so the result is cached per architecture, keyed
# by this stage's text (rootfs_stage below) and the ISO week: later builds skip it, and
# packages are at most a week old. Delete iso/work/rootfs-*.tar.zst to force a fresh one.
rootfs_stage() {
  debootstrap --arch="$ARCH" --variant=minbase --components=main,non-free-firmware trixie "$root" \
    http://deb.debian.org/debian >/dev/null
  mounts

  # Build-time speed-ups, undone below: no fsync in dpkg, no services started, and
  # update-initramfs diverted to a no-op so the kernels, plymouth and live-boot do not
  # each rebuild the initramfs (step 2 builds it once).
  echo force-unsafe-io > "$root/etc/dpkg/dpkg.cfg.d/zz-build-unsafe-io"
  printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"
  chmod +x "$root/usr/sbin/policy-rc.d"
  chroot "$root" dpkg-divert --quiet --local --rename --add /usr/sbin/update-initramfs
  printf '#!/bin/sh\nexit 0\n' > "$root/usr/sbin/update-initramfs"
  chmod +x "$root/usr/sbin/update-initramfs"
  # Kept out of the image: docs (except copyright files), man pages, translations.
  printf '%s\n' 'path-exclude /usr/share/doc/*' 'path-include /usr/share/doc/*/copyright' \
    'path-exclude /usr/share/man/*' 'path-exclude /usr/share/info/*' \
    'path-exclude /usr/share/locale/*' 'path-include /usr/share/locale/locale.alias' \
    'path-exclude /usr/share/lintian/*' > "$root/etc/dpkg/dpkg.cfg.d/01-allod-nodoc"
  echo 'Acquire::Languages "none";' > "$root/etc/apt/apt.conf.d/99-allod-nolang"
  # CA certificates first: the Docker and Raspberry Pi repositories below use HTTPS.
  cp /etc/resolv.conf "$root/etc/resolv.conf"
  chroot "$root" apt-get update -qq
  chroot "$root" apt-get install -y -qq --no-install-recommends ca-certificates >/dev/null

  if [ "$ARCH" = arm64 ]; then
    # Raspberry Pi kernels (Pi 4: rpi-v8, Pi 5: rpi-2712) and boot firmware. The archive's
    # raspberrypi.gpg.key still has SHA-1 bindings, which trixie's apt rejects since
    # 2026-02; the keyring package (pinned by checksum) carries the re-signed key.
    keydeb=raspberrypi-archive-keyring_2025.1+rpt1_all.deb
    curl -fsSLo "/tmp/$keydeb" "https://archive.raspberrypi.com/debian/pool/main/r/raspberrypi-archive-keyring/$keydeb"
    echo "2e727149d7acb8cc7f604e66d0049161039c8aa1eaf1175e54f9e69d963d60e4  /tmp/$keydeb" | sha256sum -c --quiet
    dpkg-deb -x "/tmp/$keydeb" /tmp/raspi-keyring
    cp /tmp/raspi-keyring/usr/share/keyrings/raspberrypi-archive-keyring.pgp "$root/usr/share/keyrings/"
    echo "deb [signed-by=/usr/share/keyrings/raspberrypi-archive-keyring.pgp] https://archive.raspberrypi.com/debian trixie main" \
      > "$root/etc/apt/sources.list.d/raspi.list"
    kernel="linux-image-rpi-v8 linux-image-rpi-2712 raspi-firmware firmware-brcm80211"
    mkdir -p "$root/boot/firmware"
  else
    # Common Ethernet and Wi-Fi chips in PCs and mini PCs.
    kernel="linux-image-amd64 firmware-realtek firmware-iwlwifi firmware-atheros firmware-mediatek"
  fi
  install -m 0755 -d "$root/etc/apt/keyrings"
  curl -fsSL https://download.docker.com/linux/debian/gpg -o "$root/etc/apt/keyrings/docker.asc"
  echo "deb [arch=$ARCH signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian trixie stable" \
    > "$root/etc/apt/sources.list.d/docker.list"

  echo 'man-db man-db/auto-update boolean false' | chroot "$root" debconf-set-selections
  chroot "$root" apt-get update -qq
  # shellcheck disable=SC2086
  chroot "$root" apt-get install -y -qq --no-install-recommends $kernel \
    live-boot systemd-sysv systemd-resolved systemd-timesyncd udev kmod \
    dbus iproute2 ca-certificates curl gnupg fdisk e2fsprogs zstd util-linux busybox \
    network-manager wpasupplicant wireless-regdb iw dnsmasq-base jq socat \
    avahi-daemon libnss-mdns firmware-linux-free less nano \
    plymouth libpam-systemd cage fonts-dejavu-core libgl1-mesa-dri extrepo \
    docker-ce docker-ce-cli containerd.io docker-compose-plugin >/dev/null
  # LibreWolf (kiosk browser) from its own repository; extrepo carries the source and key.
  chroot "$root" extrepo enable librewolf >/dev/null
  chroot "$root" apt-get update -qq
  chroot "$root" apt-get install -y -qq --no-install-recommends librewolf >/dev/null
  chroot "$root" apt-get clean
  rm -rf "$root"/var/lib/apt/lists/*

  rm -f "$root/usr/sbin/policy-rc.d" "$root/usr/sbin/update-initramfs" \
    "$root/etc/dpkg/dpkg.cfg.d/zz-build-unsafe-io"
  chroot "$root" dpkg-divert --quiet --local --rename --remove /usr/sbin/update-initramfs
  umounts
}

key=$( { echo "$ARCH"; sed -n '/^rootfs_stage() {/,/^}/p' "$0"; } | sha256sum | cut -c1-12)-$(date +%G-W%V)
cache="$iso/work/rootfs-$ARCH-$key.tar.zst"
mkdir -p "$root"
if [ -f "$cache" ]; then
  echo "rootfs: from cache $(basename "$cache")"
  zstd -dc -T0 "$cache" | tar -xpf - -C "$root" --numeric-owner --xattrs --xattrs-include='*'
else
  rootfs_stage
  echo "rootfs: caching $(basename "$cache")"
  rm -f "$iso/work/rootfs-$ARCH-"*.tar.zst
  tar -cpf - -C "$root" --numeric-owner --xattrs --xattrs-include='*' . \
    | zstd -T0 -3 -q -o "$cache.tmp"
  mv "$cache.tmp" "$cache"
fi
mounts
if [ "$ARCH" = arm64 ]; then
  # live-boot finds its squashfs on the FAT boot partition.
  printf 'vfat\nnls_cp437\nnls_ascii\nnls_utf8\n' >> "$root/etc/initramfs-tools/modules"
fi

# 2. Allod files and system config.
cp -a "$iso/overlay/." "$root/"
theme="$root/usr/share/plymouth/themes/allod"
rsvg-convert "$theme/logo.svg" -o "$theme/logo.png"
echo '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12"><circle cx="6" cy="6" r="6" fill="#f7931a"/></svg>' \
  | rsvg-convert -o "$theme/dot.png"
mkdir -p "$root/opt/allod"
cp -a "$iso/work/stack" "$root/opt/allod/stack"
mkdir -p "$root/opt/allod/images"
cp "$iso/work/images-$ARCH/"*.tar.zst "$iso/work/images-$ARCH/images.id" "$root/opt/allod/images/"
cp "$iso/work/images-$ARCH/version" "$root/opt/allod/version"
echo allod > "$root/etc/hostname"
printf '127.0.0.1\tlocalhost\n127.0.1.1\tallod\n' > "$root/etc/hosts"
ln -sf /run/systemd/resolve/stub-resolv.conf "$root/etc/resolv.conf"

chroot "$root" sh -eu <<'CHROOT'
chmod 0755 /usr/local/sbin/* /usr/local/bin/* /usr/share/allod-portal/cgi-bin/net
# uid 1000 stays free: it is the containers' user, which owns the network helper socket.
useradd -m -u 1001 -G video,render,input -s /usr/sbin/nologin kiosk
plymouth-set-default-theme allod
# tty1: kiosk browser. tty2 (Ctrl+Alt+F2): root console with the stack status.
# allod-boot-report is not enabled; run it by hand to print boot timing.
# Networking: NetworkManager (Ethernet DHCP, Wi-Fi), the host network helper, the
# boot-medium Wi-Fi file and the setup hotspot (SPEC §2.3.10).
systemctl enable NetworkManager systemd-resolved systemd-timesyncd avahi-daemon \
  allod-data.service allod-stack.service allod-kiosk.service getty@tty2.service allod-debug.service \
  allod-net.service allod-netconfig.service allod-hotspot.service >/dev/null 2>&1
# Docker starts only once the data disk is mounted (drop-in in the overlay), and only
# when something needs it; allod-stack pulls it in.
systemctl disable docker.service docker.socket containerd.service >/dev/null 2>&1 || true
# Skip waiting for network-online; the stack needs no network to start.
# (docker.service waits for network-online.target; without a network that would add 30 s.)
systemctl mask systemd-networkd.service systemd-networkd-wait-online.service \
  NetworkManager-wait-online.service ldconfig.service >/dev/null 2>&1
passwd -l root >/dev/null
# Built once, here: the package installs skipped it.
update-initramfs -c -k all >/dev/null
CHROOT

umounts
trap - EXIT
mkdir -p /build/live
mksquashfs "$root" /build/live/filesystem.squashfs -comp zstd -Xcompression-level 15 \
  -e boot -noappend -quiet

finish() { chmod 0644 "$1"; ls -lh "$1"; }

# 3a. PC: hybrid BIOS/UEFI ISO. The snapshot variant carries the file in /snapshot.
if [ "$ARCH" = amd64 ]; then
  rm -rf "$tree" && mkdir -p "$tree/live" "$tree/boot/grub"
  cp "$root"/boot/vmlinuz-* "$tree/live/vmlinuz"
  cp "$root"/boot/initrd.img-* "$tree/live/initrd.img"
  ln /build/live/filesystem.squashfs "$tree/live/filesystem.squashfs"
  cp "$iso/grub.cfg" "$tree/boot/grub/grub.cfg"
  # A small writable FAT partition (ALLODCFG) after the ISO data, for allod-network.txt:
  # visible and editable on any computer once the image is written to a USB stick.
  rm -f /build/cfg.fat
  mkfs.vfat -n ALLODCFG -C /build/cfg.fat 8192 >/dev/null
  mcopy -i /build/cfg.fat "$iso/allod-network.txt" ::/
  grub-mkrescue -o "$iso/out/allod-amd64.iso" "$tree" -- -volid ALLOD \
    -append_partition 3 0x0c /build/cfg.fat >/dev/null 2>&1
  finish "$iso/out/allod-amd64.iso"
  if [ -n "$snapshot" ]; then
    # ISO level 3 allows files over 4 GiB; -map adds the snapshot without copying it.
    grub-mkrescue -o "$iso/out/allod-amd64-snapshot.iso" "$tree" -- -volid ALLOD \
      -append_partition 3 0x0c /build/cfg.fat \
      -compliance iso_9660_level=3 -map "$snapshot" "/snapshot/$(basename "$snapshot")" >/dev/null 2>&1
    finish "$iso/out/allod-amd64-snapshot.iso"
  fi
  exit 0
fi

# 3b. Raspberry Pi: MBR disk image. Partition 1 (FAT32, ALLODBOOT): firmware, kernels,
# initramfs, config.txt and the squashfs. Snapshot variant: partition 2 (ext4,
# ALLOD_SNAP, read-only at runtime) holds the snapshot, which is too big for FAT32.
boot=/build/boot
rm -rf "$boot" && mkdir -p "$boot/live"
cp -a "$root/boot/firmware/." "$boot/" 2>/dev/null || true
for f in "$root"/usr/lib/raspi-firmware/* "$root"/usr/lib/linux-image-*-rpi-v8/broadcom/*.dtb \
         "$root"/usr/lib/linux-image-*-rpi-2712/broadcom/*.dtb; do
  [ -f "$f" ] && cp "$f" "$boot/"
done
ov=$(ls -d "$root"/usr/lib/linux-image-*-rpi-2712/overlays 2>/dev/null | head -1 || true)
[ -n "$ov" ] && cp -a "$ov" "$boot/"
cp "$root"/boot/vmlinuz-*-rpi-v8 "$boot/kernel8.img"
cp "$root"/boot/initrd.img-*-rpi-v8 "$boot/initramfs8"
cp "$root"/boot/vmlinuz-*-rpi-2712 "$boot/kernel_2712.img"
cp "$root"/boot/initrd.img-*-rpi-2712 "$boot/initramfs_2712"
cp "$iso/rpi/config.txt" "$iso/rpi/cmdline.txt" "$iso/allod-network.txt" "$boot/"
ln /build/live/filesystem.squashfs "$boot/live/filesystem.squashfs"

mib() { echo $(( ($(du -sb "$1" | cut -f1) + 1048575) / 1048576 )); }
boot_mib=$(( $(mib "$boot") * 110 / 100 + 64 ))
rm -f /build/boot.fat
mkfs.vfat -F 32 -n ALLODBOOT -C /build/boot.fat $((boot_mib * 1024)) >/dev/null
mcopy -s -i /build/boot.fat "$boot"/* ::/

image() { # output, with or without the snapshot partition
  img=$1 snap_mib=${2:-0}
  rm -f "$img"
  truncate -s $(( (1 + boot_mib + snap_mib) * 1048576 )) "$img"
  if [ "$snap_mib" -gt 0 ]; then
    printf 'label: dos\nstart=2048, size=%s, type=c, bootable\nstart=%s, type=83\n' \
      $((boot_mib * 2048)) $(((1 + boot_mib) * 2048)) | sfdisk -q "$img"
    dd if=/build/snap.ext4 of="$img" bs=1M seek=$((1 + boot_mib)) conv=notrunc,sparse status=none
  else
    printf 'label: dos\nstart=2048, size=%s, type=c, bootable\n' $((boot_mib * 2048)) | sfdisk -q "$img"
  fi
  dd if=/build/boot.fat of="$img" bs=1M seek=1 conv=notrunc status=none
  finish "$img"
}
image "$iso/out/allod-rpi-arm64.img"
if [ -n "$snapshot" ]; then
  snap_mib=$(( $(mib "$snapshot") + 128 ))
  rm -f /build/snap.ext4
  mkfs.ext4 -q -L ALLOD_SNAP -m 0 -d /snapshot /build/snap.ext4 "${snap_mib}M"
  image "$iso/out/allod-rpi-arm64-snapshot.img" "$snap_mib"
  rm -f /build/snap.ext4
fi
