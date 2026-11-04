# Shared helpers of the Allod appliance scripts (sourced, POSIX sh).

# Name of the disk holding the boot medium (e.g. sdb), empty if unknown. Never offered for data.
boot_disk() {
  _src=$(findmnt -no SOURCE /run/live/medium 2>/dev/null || true)
  [ -n "$_src" ] || return 0
  _d=$(lsblk -no PKNAME "$_src" 2>/dev/null | head -1)
  [ -n "$_d" ] || _d=$(basename "$_src")
  echo "$_d"
}

# Settings folder on the boot medium: the Raspberry Pi's boot partition, or the PC image's
# small ALLODCFG partition. Prints nothing when there is none (e.g. the ISO on a CD).
medium_dir() {
  if [ -f /run/live/medium/config.txt ]; then echo /run/live/medium; return 0; fi
  _dev=$(blkid -L ALLODCFG 2>/dev/null) || return 0
  mkdir -p /run/allod-cfg
  if ! mountpoint -q /run/allod-cfg; then
    mount -t vfat -o rw,umask=077 "$_dev" /run/allod-cfg 2>/dev/null || return 0
  fi
  echo /run/allod-cfg
}

# Writes stdin to file $2 in the medium folder $1 (the Pi's boot partition is mounted
# read-only and is remounted for the write).
medium_write() {
  if [ "$1" = /run/live/medium ]; then mount -o remount,rw "$1"; fi
  cat > "$1/$2.tmp" && mv "$1/$2.tmp" "$1/$2"
  sync
  if [ "$1" = /run/live/medium ]; then mount -o remount,ro "$1"; fi
  return 0
}

# Value of KEY in a KEY=value file (Windows line endings and surrounding quotes allowed).
conf_value() { # file key
  tr -d '\r' < "$1" | sed -n "s/^[[:space:]]*$2[[:space:]]*=//p" | tail -1 \
    | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/'
}

# ---- Data location (allod-choose-data on the console, allod-data-api for the kiosk) ----

ALLOD_DATA_MIN=$((10 * 1000 * 1000 * 1000))
ALLOD_DATA_LABEL=ALLOD_DATA

# Free bytes of a Linux filesystem, from a read-only mount that does not replay its
# journal (the other system's files stay exactly as they are).
free_bytes() { # dev fstype
  case $2 in
    ext4|ext3) _o=ro,noload ;;
    xfs) _o=ro,norecovery ;;
    *) _o=ro ;;
  esac
  mkdir -p /run/allod-probe
  mount -t "$2" -o "$_o" "$1" /run/allod-probe 2>/dev/null || { echo 0; return 0; }
  df -B1 --output=avail /run/allod-probe | tail -1 | tr -d ' '
  umount /run/allod-probe
}

# Places Allod can keep its data, one per line:
#   kind|device|size_bytes|fstype|label|free_bytes|start|sectors
# kind: dir (a folder /allod-data on an existing Linux filesystem; its files are kept),
#       format (a partition to format: erases it), new (free space on a disk: a new
#       partition, start/sectors in the disk's logical sectors), disk (a blank disk).
data_options() {
  # Safest first: keep files (dir), unused space (new), blank disk, then erasing (format).
  data_options_scan | awk -F'|' 'BEGIN { p["dir"]=0; p["new"]=1; p["disk"]=2; p["format"]=3 }
    { print p[$1] "|" $0 }' | sort -s -t'|' -k1,1n | cut -d'|' -f2-
}

data_options_scan() {
  _boot=$(boot_disk)
  lsblk -J -b -p -l -o NAME,TYPE,SIZE,FSTYPE,LABEL,PKNAME,RO 2>/dev/null \
    | jq -r '.blockdevices[] | [.name, .type, (.size|tostring), (.fstype // ""),
              (.label // "" | gsub("[|\n]"; " ")), (.pkname // ""), (.ro|tostring)] | join("|")' \
    | while IFS='|' read -r _name _type _size _fs _lbl _parent _ro; do
        [ "$_ro" = false ] || [ "$_ro" = 0 ] || continue
        [ "$(basename "${_parent:-$_name}")" = "$_boot" ] && continue
        [ "$_size" -ge "$ALLOD_DATA_MIN" ] 2>/dev/null || continue
        case $_type in
          part)
            case $_fs in swap|crypto_LUKS|LVM2_member|linux_raid_member|zfs_member) continue ;; esac
            case $_fs in
              ext4|ext3|xfs|btrfs)
                _free=$(free_bytes "$_name" "$_fs")
                if [ "$_free" -ge "$ALLOD_DATA_MIN" ] 2>/dev/null; then
                  echo "dir|$_name|$_size|$_fs|$_lbl|$_free||"
                fi ;;
            esac
            echo "format|$_name|$_size|$_fs|$_lbl|||" ;;
          disk)
            if [ "$(lsblk -n "$_name" | wc -l)" -eq 1 ] && ! blkid -p "$_name" >/dev/null 2>&1; then
              echo "disk|$_name|$_size||||"
            fi
            # The largest free region of a partitioned disk (sfdisk counts in the disk's
            # own logical sectors, 512 or 4096 bytes).
            _region=$(sfdisk -F "$_name" 2>/dev/null | awk '$1 ~ /^[0-9]+$/ && NF >= 3 {print $1, $3}' \
              | sort -k2 -n | tail -1)
            if [ -n "$_region" ]; then
              _start=${_region% *} _sectors=${_region#* }
              _bytes=$((_sectors * $(blockdev --getss "$_name")))
              if [ "$_bytes" -ge "$ALLOD_DATA_MIN" ]; then
                echo "new|$_name|$_bytes||||$_start|$_sectors"
              fi
            fi ;;
        esac
      done
  return 0
}

# Removes a saved choice (allod-data.conf) from the boot medium.
data_forget_saved() {
  _cfg=$(medium_dir)
  if [ -n "$_cfg" ] && [ -f "$_cfg/allod-data.conf" ]; then
    if [ "$_cfg" = /run/live/medium ]; then mount -o remount,rw "$_cfg"; fi
    rm -f "$_cfg/allod-data.conf"
    if [ "$_cfg" = /run/live/medium ]; then mount -o remount,ro "$_cfg"; fi
  fi
  return 0
}

# Prepares one data_options line (no questions asked; callers confirm erasing first).
# Prints progress; on failure prints the reason and returns 1.
data_apply() { # kind dev start sectors
  case $1 in
    dir)
      _uuid=$(blkid -s UUID -o value "$2")
      [ -n "$_uuid" ] || { echo "$2 has no filesystem UUID"; return 1; }
      _cfg=$(medium_dir)
      if [ -n "$_cfg" ]; then
        printf '# Where Allod keeps its data (written by allod-choose-data).\nALLOD_DATA_UUID=%s\nALLOD_DATA_DIR=/allod-data\n' "$_uuid" \
          | medium_write "$_cfg" allod-data.conf
      else
        echo "No writable settings partition on the boot medium: the choice lasts until reboot."
        mkdir -p /run/allod-host
        mountpoint -q /run/allod-host || mount -o noatime "$2" /run/allod-host || return 1
        mkdir -p /run/allod-host/allod-data /var/lib/allod
        mount --bind /run/allod-host/allod-data /var/lib/allod || return 1
      fi ;;
    format)
      echo "Formatting $2..."
      if ! { wipefs -aq "$2" && mkfs.ext4 -q -F -L "$ALLOD_DATA_LABEL" "$2"; }; then
        echo "could not format $2"; return 1
      fi
      data_forget_saved ;;
    new)
      echo "Creating a partition in the free space of $2..."
      _before=$(lsblk -lnpo NAME,TYPE "$2" | awk '$2=="part" {print $1}')
      printf '%s,%s,L\n' "$3" "$4" | sfdisk --append --no-reread -q "$2" || { echo "could not create the partition"; return 1; }
      partx -a "$2" 2>/dev/null || partprobe "$2" 2>/dev/null || true
      udevadm settle
      _part=""
      for _p in $(lsblk -lnpo NAME,TYPE "$2" | awk '$2=="part" {print $1}'); do
        echo "$_before" | grep -qx "$_p" || _part=$_p
      done
      [ -n "$_part" ] || { echo "the new partition did not appear; reboot and choose again"; return 1; }
      echo "Formatting $_part..."
      mkfs.ext4 -q -F -L "$ALLOD_DATA_LABEL" "$_part" || { echo "could not format $_part"; return 1; }
      data_forget_saved ;;
    disk)
      echo "Formatting $2..."
      /usr/local/sbin/allod-format-disk --yes "$2" >/dev/null || { echo "could not format $2"; return 1; }
      data_forget_saved ;;
    *) echo "unknown choice"; return 1 ;;
  esac
  return 0
}

# Mounts the chosen location and starts the stack in the background (loading the images
# on a first start takes a few minutes; the kiosk page waits for the web UI).
data_activate() {
  systemctl restart allod-data.service || { echo "could not mount the data location (journalctl -u allod-data)"; return 1; }
  systemctl start --no-block allod-stack.service
  # A kiosk started before there was a data location keeps its browser profile in RAM.
  # Restart it in a few seconds (after this answer reached its page) so the profile moves
  # to the disk; it reopens the start page, which waits for the web UI.
  if systemctl -q is-active allod-kiosk.service && ! mountpoint -q /home/kiosk/profile; then
    systemd-run --quiet --on-active=3 systemctl restart allod-kiosk.service || true
  fi
  return 0
}
