#!/bin/sh
# Boot the ISO in QEMU with a blank 60 GB data disk. Web UI: http://127.0.0.1:18080
# Usage: iso/run-qemu.sh [--gui|--vnc] [--uefi] [--cdrom] [--snapshot]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
disk="$here/work/data.qcow2"
[ -f "$disk" ] || qemu-img create -q -f qcow2 "$disk" 60G

# Default: ISO attached as a disk, like a USB stick written with dd.
# --cdrom: attach as a CD drive.  --uefi: boot with OVMF instead of SeaBIOS.
# --gui: show the display (kiosk) in a window.
# --snapshot: boot allod-amd64-snapshot.iso (with the UTXO snapshot) instead.
# --vnc: show the display in a browser (noVNC) at http://127.0.0.1:6080/
iso="$here/out/allod-amd64.iso"
for a in "$@"; do [ "$a" = --snapshot ] && iso="$here/out/allod-amd64-snapshot.iso"; done
vnc="" fw="" display="-display none" boot="-drive file=$iso,if=virtio,format=raw,readonly=on"
for a in "$@"; do
  case $a in
    --uefi) fw="-bios /usr/share/ovmf/OVMF.fd" ;;
    --cdrom) boot="-cdrom $iso -boot d" ;;
    --gui) display="-display gtk,zoom-to-fit=on" ;;
    --vnc) display="-display none -vnc 127.0.0.1:0,websocket=on"; vnc=1 ;;
  esac
done

if [ -n "$vnc" ]; then
  novnc="$here/work/novnc"
  if [ ! -f "$novnc/vnc.html" ]; then
    mkdir -p "$novnc"
    curl -fsSL https://github.com/novnc/noVNC/archive/refs/tags/v1.5.0.tar.gz | tar -xz -C "$novnc" --strip-components=1
  fi
  python3 -m http.server 6080 --bind 127.0.0.1 --directory "$novnc" >/dev/null 2>&1 &
  trap 'kill $! 2>/dev/null' EXIT
  echo "Display: http://127.0.0.1:6080/vnc.html?host=127.0.0.1&port=5700&path=&autoconnect=1&resize=scale" >&2
fi
echo "Ctrl-A X: kill the VM.  Ctrl-A C, then 'system_powerdown': clean shutdown." >&2

# shellcheck disable=SC2086
qemu-system-x86_64 -enable-kvm -cpu host -smp 4 -m 4096 $fw $display -vga virtio \
  $boot \
  -drive file="$disk",if=virtio,format=qcow2 \
  -nic user,model=virtio-net-pci,hostfwd=tcp::18080-:8029 \
  -monitor unix:"$here/work/monitor.sock",server,nowait \
  -serial mon:stdio
