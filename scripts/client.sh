#!/usr/bin/env sh
set -eu

SOCKET_PATH="${1:-/tmp/gongd.sock}"

if command -v socat >/dev/null 2>&1; then
  exec socat - "UNIX-CONNECT:${SOCKET_PATH}"
fi

if command -v nc >/dev/null 2>&1; then
  exec nc -U "${SOCKET_PATH}"
fi

echo "Neither socat nor nc is available." >&2
exit 1
