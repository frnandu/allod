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

# 1. Minimal Debian.
debootstrap --arch="$ARCH" --variant=minbase --components=main,non-free-firmware trixie "$root" \
  http://deb.debian.org/debian >/dev/null

mount -t proc proc "$root/proc"
mount -t sysfs sys "$root/sys"
mount --bind /dev "$root/dev"
mount -t devpts devpts "$root/dev/pts" 2>/dev/null || true
trap 'umount -l "$root/dev/pts" "$root/proc" "$root/sys" "$root/dev" 2>/dev/null || true' EXIT

# Don't start services while installing packages.
printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"
chmod +x "$root/usr/sbin/policy-rc.d"

if [ "$ARCH" = arm64 ]; then
  # Raspberry Pi kernels (Pi 4: rpi-v8, Pi 5: rpi-2712) and boot firmware.
  curl -fsSL https://archive.raspberrypi.com/debian/raspberrypi.gpg.key \
    | gpg --dearmor > "$root/usr/share/keyrings/raspberrypi-archive-keyring.gpg"
  echo "deb [signed-by=/usr/share/keyrings/raspberrypi-archive-keyring.gpg] http://archive.raspberrypi.com/debian trixie main" \
    > "$root/etc/apt/sources.list.d/raspi.list"
  kernel="linux-image-rpi-v8 linux-image-rpi-2712 raspi-firmware"
  mkdir -p "$root/boot/firmware"
  # live-boot finds its squashfs on the FAT boot partition.
  printf 'vfat\nnls_cp437\nnls_ascii\nnls_utf8\n' >> "$root/etc/initramfs-tools/modules"
else
  kernel="linux-image-amd64 firmware-realtek"
fi

cp /etc/resolv.conf "$root/etc/resolv.conf"
chroot "$root" env ARCH="$ARCH" KERNEL="$kernel" sh -eu <<'EOF'
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# shellcheck disable=SC2086
apt-get install -y -qq --no-install-recommends $KERNEL \
  live-boot systemd-sysv systemd-resolved systemd-timesyncd udev kmod \
  dbus iproute2 ca-certificates curl gnupg fdisk e2fsprogs zstd util-linux busybox \
  avahi-daemon libnss-mdns firmware-linux-free less nano \
  plymouth libpam-systemd cage fonts-dejavu-core libgl1-mesa-dri extrepo >/dev/null

install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
echo "deb [arch=$ARCH signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian trixie stable" \
  > /etc/apt/sources.list.d/docker.list
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  docker-ce docker-ce-cli containerd.io docker-compose-plugin >/dev/null

# LibreWolf (kiosk browser) from its own repository; extrepo carries the source and key.
extrepo enable librewolf >/dev/null
apt-get update -qq
apt-get install -y -qq --no-install-recommends librewolf >/dev/null
apt-get clean
rm -rf /var/lib/apt/lists/*
EOF
rm -f "$root/usr/sbin/policy-rc.d"

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

chroot "$root" sh -eu <<'EOF'
chmod 0755 /usr/local/sbin/* /usr/local/bin/*
useradd -m -G video,render,input -s /usr/sbin/nologin kiosk
plymouth-set-default-theme allod
# tty1: kiosk browser. tty2 (Ctrl+Alt+F2): root console with the stack status.
# allod-boot-report is not enabled; run it by hand to print boot timing.
systemctl enable systemd-networkd systemd-resolved systemd-timesyncd avahi-daemon \
  allod-data.service allod-stack.service allod-kiosk.service getty@tty2.service allod-debug.service >/dev/null 2>&1
# Docker starts only once the data disk is mounted (drop-in in the overlay), and only
# when something needs it; allod-stack pulls it in.
systemctl disable docker.service docker.socket containerd.service >/dev/null 2>&1 || true
# Skip waiting for network-online; the stack needs no network to start.
systemctl mask systemd-networkd-wait-online.service ldconfig.service >/dev/null 2>&1
passwd -l root >/dev/null
update-initramfs -u -k all >/dev/null
EOF

umount -l "$root/dev/pts" "$root/proc" "$root/sys" "$root/dev" 2>/dev/null || true
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
  grub-mkrescue -o "$iso/out/allod-amd64.iso" "$tree" -- -volid ALLOD >/dev/null 2>&1
  finish "$iso/out/allod-amd64.iso"
  if [ -n "$snapshot" ]; then
    # ISO level 3 allows files over 4 GiB; -map adds the snapshot without copying it.
    grub-mkrescue -o "$iso/out/allod-amd64-snapshot.iso" "$tree" -- -volid ALLOD \
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
cp "$iso/rpi/config.txt" "$iso/rpi/cmdline.txt" "$boot/"
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
