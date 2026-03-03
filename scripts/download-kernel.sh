#!/bin/bash
set -e

OUTPUT=/var/lib/imparando/vmlinux

# Verify an existing file is actually an ELF binary before skipping.
if [ -f "$OUTPUT" ] && file "$OUTPUT" | grep -q ELF; then
    echo "Kernel already exists at $OUTPUT, skipping download."
    exit 0
fi

mkdir -p /var/lib/imparando

# Firecracker's quickstart kernel (x86_64 uncompressed vmlinux ELF, kernel 5.10).
# The GitHub release asset URL is a 404 — use the canonical S3 location instead.
URL="https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin"

echo "Downloading Firecracker kernel..."
curl -L -o "$OUTPUT" "$URL"

# Confirm we got a real ELF.
if ! file "$OUTPUT" | grep -q ELF; then
    echo "ERROR: Downloaded file is not an ELF binary. Download may have failed."
    rm -f "$OUTPUT"
    exit 1
fi

chmod 644 "$OUTPUT"
echo "Kernel downloaded successfully to $OUTPUT"
