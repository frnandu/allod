# Console banner: where to open the UI and what the stack is doing.
if [ "$(id -u)" = 0 ] && [ -t 1 ]; then
  echo
  echo "Allod appliance"
  for ip in $(ip -4 -o addr show scope global | awk '{print $4}' | cut -d/ -f1); do
    echo "  Web UI: http://$ip:8029   (or http://allod.local:8029)"
  done
  if mountpoint -q /var/lib/allod; then
    echo "  Data:   $(findmnt -no SOURCE /var/lib/allod) $(df -h --output=avail /var/lib/allod | tail -1 | tr -d ' ') free"
    echo "  Stack:  $(systemctl is-active allod-stack)   logs: journalctl -u allod-stack; docker compose --project-directory /var/lib/allod/stack logs -f"
  else
    echo "  No data disk. The stack is not running."
    echo "  Pick a disk:  lsblk   then   allod-format-disk /dev/sdX"
  fi
  echo
fi
