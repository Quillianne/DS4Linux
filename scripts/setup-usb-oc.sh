#!/bin/sh
set -eu

vid="${1:-${DS4LINUX_USB_OC_VID:-054c}}"
pid="${2:-${DS4LINUX_USB_OC_PID:-0ce6}}"
binterval="${3:-${DS4LINUX_USB_OC_BINTERVAL:-4}}"

validate_hex_id() {
  name="$1"
  value="$2"
  case "$value" in
    ????)
      ;;
    *)
      echo "Invalid $name: expected 4 hexadecimal characters, got '$value'" >&2
      exit 1
      ;;
  esac
  case "$value" in
    *[!0123456789abcdefABCDEF]*)
      echo "Invalid $name: expected hexadecimal characters, got '$value'" >&2
      exit 1
      ;;
  esac
}

validate_hex_id "VID" "$vid"
validate_hex_id "PID" "$pid"

case "$binterval" in
  ''|*[!0123456789]*)
    echo "Invalid bInterval: expected an integer from 1 to 16, got '$binterval'" >&2
    exit 1
    ;;
esac

if [ "$binterval" -lt 1 ] || [ "$binterval" -gt 16 ]; then
  echo "Invalid bInterval: expected an integer from 1 to 16, got '$binterval'" >&2
  exit 1
fi

override="$vid:$pid:$binterval"

pkexec sh -c '
set -eu
override="$1"

modprobe usb_oc

install -d -m 0755 /etc/modules-load.d /etc/modprobe.d

tmp_modules="$(mktemp)"
printf "%s\n" "usb_oc" > "$tmp_modules"
install -m 0644 "$tmp_modules" /etc/modules-load.d/usb_oc.conf
rm -f "$tmp_modules"

tmp_modprobe="$(mktemp)"
printf "%s\n" "options usb_oc interrupt_interval_override=$override" > "$tmp_modprobe"
install -m 0644 "$tmp_modprobe" /etc/modprobe.d/usb_oc.conf
rm -f "$tmp_modprobe"

printf "%s" "$override" > /sys/module/usb_oc/parameters/interrupt_interval_override
' sh "$override"

cat <<EOF
usb_oc configured for $override

Persistent files:
  /etc/modules-load.d/usb_oc.conf
  /etc/modprobe.d/usb_oc.conf

The module must still be installed by your distro package, for example usb_oc-dkms.
EOF
