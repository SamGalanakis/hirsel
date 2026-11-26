#!/bin/bash
# The QUIC Interop Runner's entry point for the Hirsel endpoint: maps ROLE, TESTCASE,
# REQUESTS and /certs to endpoint.hirsel's arguments. Exit 127 marks a case Hirsel does
# not take part in (the runner's README).
set -e
/setup.sh

case "$TESTCASE" in
  handshake|transfer|retry|multiconnect|ecn) ;;
  # The client cannot yet restrict its offer to ChaCha20 or start a key update.
  chacha20) [ "$ROLE" = server ] || exit 127 ;;
  *) exit 127 ;;
esac

mkdir -p /hq /logs
status=0
if [ "$ROLE" = server ]; then
  # The runner's chain, leaf first, as DER, and its key as PKCS#8.
  awk '/BEGIN CERTIFICATE/ { n++ } { print > ("/hq/c" n ".pem") }' /certs/cert.pem
  count=$(ls /hq/c*.pem | wc -l)
  for i in $(seq 1 "$count"); do
    openssl x509 -in "/hq/c$i.pem" -outform DER -out "/hq/cert-$((i - 1)).der"
  done
  openssl pkcs8 -topk8 -nocrypt -in /certs/priv.key -outform DER -out /hq/key.der
  /endpoint server "$TESTCASE" "$count" > /logs/server.log 2>&1 || status=$?
else
  /wait-for-it.sh sim:57832 -s -t 30
  first=${REQUESTS%% *}
  authority=${first#https://}
  authority=${authority%%/*}
  host=${authority%:*}
  port=${authority##*:}
  ip=$(getent ahostsv4 "$host" | head -1 | cut -d' ' -f1)
  # The leaf is the first certificate in cert.pem; Hirsel pins its SHA-256 (QUIC-3).
  pin=$(openssl x509 -in /certs/cert.pem -outform DER | sha256sum | cut -d' ' -f1)
  names=()
  for r in $REQUESTS; do names+=("${r##*/}"); done
  /endpoint client "$TESTCASE" "$ip" "$port" "$host" "$pin" "${names[@]}" > /logs/client.log 2>&1 || status=$?
  # std.file writes 0600; the runner reads the downloads as another user.
  chmod -R a+r /downloads
fi
[ "$status" = 0 ] || exit 1
