#!/bin/sh
set -eu

systemctl status --no-pager --lines=20 ds4linuxd.service
