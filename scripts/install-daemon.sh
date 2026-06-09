#!/bin/sh
set -eu

repo_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_user="${DS4LINUX_TARGET_USER:-}"

if [ -z "$target_user" ]; then
  target_user="${SUDO_USER:-}"
fi

if [ -z "$target_user" ] || [ "$target_user" = "root" ]; then
  repo_owner="$(stat -c '%U' "$repo_dir" 2>/dev/null || true)"
  if [ -n "$repo_owner" ] && [ "$repo_owner" != "UNKNOWN" ] && [ "$repo_owner" != "root" ]; then
    target_user="$repo_owner"
  fi
fi

if [ -z "$target_user" ]; then
  target_user="$(id -un)"
fi

case "$target_user" in
  ""|*[!A-Za-z0-9_.-]*)
    echo "Invalid target user: $target_user" >&2
    exit 1
    ;;
esac

socket_group="${DS4LINUX_SOCKET_GROUP:-}"
if [ -z "$socket_group" ]; then
  socket_group="$(id -gn "$target_user" 2>/dev/null || true)"
fi
if [ -z "$socket_group" ]; then
  socket_group="users"
fi

case "$socket_group" in
  ""|*[!A-Za-z0-9_.-]*)
    echo "Invalid socket group: $socket_group" >&2
    exit 1
    ;;
esac

cargo build --release -p ds4linuxd --manifest-path "$repo_dir/Cargo.toml"

pkexec sh -c '
set -eu
repo_dir="$1"
target_user="$2"
socket_group="$3"

install -D -m 0755 "$repo_dir/target/release/ds4linuxd" /usr/local/bin/ds4linuxd
install -D -m 0644 "$repo_dir/systemd/ds4linuxd.service" /etc/systemd/system/ds4linuxd.service
install -d -m 0755 /etc/ds4linux
if [ ! -f /etc/ds4linux/config.json ]; then
  tmp_config="$(mktemp)"
  sed \
    -e "s/\"target_user\": \"\"/\"target_user\": \"$target_user\"/" \
    -e "s/\"socket_group\": \"\"/\"socket_group\": \"$socket_group\"/" \
    "$repo_dir/config/default-config.json" > "$tmp_config"
  install -m 0644 "$tmp_config" /etc/ds4linux/config.json
  rm -f "$tmp_config"
fi
systemctl daemon-reload
' sh "$repo_dir" "$target_user" "$socket_group"

echo "Installed ds4linuxd. Start it with:"
echo "  pkexec systemctl enable --now ds4linuxd.service"
echo "Config target_user=$target_user socket_group=$socket_group"
