#!/bin/sh
set -eu

pkexec sh -c '
set -eu
systemctl disable --now ds4linuxd.service 2>/dev/null || true
rm -f /etc/systemd/system/ds4linuxd.service
rm -f /usr/local/bin/ds4linuxd
rm -f /etc/udev/rules.d/99-ds4linux-hide-physical.rules
systemctl daemon-reload
udevadm control --reload
udevadm trigger --action=change --subsystem-match=input || true
udevadm trigger --action=change --subsystem-match=hidraw || true
'

echo "Removed ds4linuxd binary, service, and hide rule. /etc/ds4linux/config.json was kept."
