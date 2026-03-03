#!/bin/bash
set -e

KERNEL_VERSION=v1.7.0
OUTPUT=/var/lib/imparando/vmlinux

if [ -f "$OUTPUT" ]; then
    echo "Kernel already exists at $OUTPUT, skipping download."
    exit 0
fi

mkdir -p /var/lib/imparando

URL="https://github.com/firecracker-microvm/firecracker/releases/download/${KERNEL_VERSION}/vmlinux-5.10-x86_64.bin"

echo "Downloading Firecracker kernel ${KERNEL_VERSION}..."
curl -L -o "$OUTPUT" "$URL"

chmod 644 "$OUTPUT"

echo "Kernel downloaded successfully to $OUTPUT"
