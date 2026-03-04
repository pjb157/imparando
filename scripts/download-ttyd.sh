#!/bin/bash
set -e

OUTPUT=/var/lib/imparando/ttyd
VERSION=1.7.7

if [ -f "$OUTPUT" ] && [ -x "$OUTPUT" ]; then
    echo "ttyd already exists at $OUTPUT, skipping download."
    exit 0
fi

mkdir -p /var/lib/imparando

URL="https://github.com/tsl0922/ttyd/releases/download/${VERSION}/ttyd.x86_64"

echo "Downloading ttyd v${VERSION}..."
curl -L -o "$OUTPUT" "$URL"
chmod +x "$OUTPUT"
echo "ttyd downloaded to $OUTPUT"
