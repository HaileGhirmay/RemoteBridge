#!/usr/bin/env sh
# Check that coturn accepts credentials minted the way the website mints them,
# and rejects tampered ones. Needs `turnutils_uclient` (part of coturn) and openssl.
#
#   TURN_SHARED_SECRET=... ./test-credentials.sh turn.example.com
set -eu

HOST="${1:?usage: test-credentials.sh <turn-host>}"
SECRET="${TURN_SHARED_SECRET:?set TURN_SHARED_SECRET}"
SESSION="00000000-0000-4000-8000-000000000001"

EXPIRY=$(( $(date +%s) + 600 ))
USERNAME="${EXPIRY}:${SESSION}"
CREDENTIAL=$(printf '%s' "$USERNAME" | openssl dgst -sha1 -hmac "$SECRET" -binary | openssl base64)

echo "== valid credential (expect: allocation succeeds)"
if turnutils_uclient -u "$USERNAME" -w "$CREDENTIAL" -n 1 -m 1 -l 10 -y "$HOST" >/tmp/turn-ok.log 2>&1; then
  echo "PASS: accepted"
else
  echo "FAIL: a valid credential was refused"; tail -n 20 /tmp/turn-ok.log; exit 1
fi

echo "== tampered credential (expect: refused)"
if turnutils_uclient -u "$USERNAME" -w "${CREDENTIAL}x" -n 1 -m 1 -l 10 -y "$HOST" >/tmp/turn-bad.log 2>&1; then
  echo "FAIL: a tampered credential was accepted"; exit 1
else
  echo "PASS: refused"
fi

echo "== expired credential (expect: refused)"
OLD="$(( $(date +%s) - 60 )):${SESSION}"
OLD_CRED=$(printf '%s' "$OLD" | openssl dgst -sha1 -hmac "$SECRET" -binary | openssl base64)
if turnutils_uclient -u "$OLD" -w "$OLD_CRED" -n 1 -m 1 -l 10 -y "$HOST" >/tmp/turn-old.log 2>&1; then
  echo "FAIL: an expired credential was accepted"; exit 1
else
  echo "PASS: refused"
fi
