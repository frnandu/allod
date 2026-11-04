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
    echo "  No data location yet. The stack is not running."
  fi
  echo "  Network: nmtui (Wi-Fi), or Settings > Network in the web UI"
  echo
fi

# No data location: with a display, the kiosk on tty1 offers the choice. Without one
# (headless boot entry, no graphics), open the menu here, once per login.
if [ "$(id -u)" = 0 ] && [ "$(tty)" = /dev/tty2 ] && ! mountpoint -q /var/lib/allod \
   && [ -z "${ALLOD_CHOOSER_SHOWN:-}" ]; then
  export ALLOD_CHOOSER_SHOWN=1
  if grep -qw allod.nokiosk /proc/cmdline || ! ls /dev/dri/card* >/dev/null 2>&1; then
    /usr/local/sbin/allod-choose-data || true
    echo "(Run allod-choose-data to choose again.)"
  else
    echo "  Choose where Allod keeps its data on the screen (Ctrl+Alt+F1),"
    echo "  or here with: allod-choose-data"
    echo
  fi
fi
