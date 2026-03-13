#!/bin/bash
set -euo pipefail

PREFIX="/opt/imparando"
LINK_BIN="/usr/local/bin/imparando"
SOURCE=""

usage() {
  cat <<'EOF'
Usage: install-bundle.sh [--prefix DIR] [--link-bin PATH] <tarball-or-url>

Examples:
  sudo ./scripts/install-bundle.sh ./imparando-bundle-v0.1.0-linux-amd64.tar.gz
  sudo ./scripts/install-bundle.sh \
    https://github.com/OWNER/REPO/releases/download/v0.1.0/imparando-bundle-v0.1.0-linux-amd64.tar.gz

Installs the bundle under:
  <prefix>/releases/<bundle-name>/

Updates:
  <prefix>/current -> selected release

And optionally symlinks:
  <link-bin> -> <prefix>/current/bin/imparando
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      PREFIX="$2"
      shift 2
      ;;
    --link-bin)
      LINK_BIN="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [[ -n "$SOURCE" ]]; then
        echo "ERROR: only one tarball-or-url may be provided" >&2
        usage
        exit 1
      fi
      SOURCE="$1"
      shift
      ;;
  esac
done

if [[ -z "$SOURCE" ]]; then
  echo "ERROR: missing tarball-or-url" >&2
  usage
  exit 1
fi

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

ARCHIVE_PATH="$TMPDIR/bundle.tar.gz"
if [[ "$SOURCE" =~ ^https?:// ]]; then
  echo "Downloading bundle from $SOURCE"
  curl -fL "$SOURCE" -o "$ARCHIVE_PATH"
else
  if [[ ! -f "$SOURCE" ]]; then
    echo "ERROR: file not found: $SOURCE" >&2
    exit 1
  fi
  cp "$SOURCE" "$ARCHIVE_PATH"
fi

BASENAME="$(basename "$SOURCE")"
RELEASE_NAME="${BASENAME%.tar.gz}"
RELEASE_DIR="$PREFIX/releases/$RELEASE_NAME"
CURRENT_LINK="$PREFIX/current"

mkdir -p "$PREFIX/releases"
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"

echo "Extracting bundle to $RELEASE_DIR"
tar -C "$RELEASE_DIR" -xzf "$ARCHIVE_PATH"

ln -sfn "$RELEASE_DIR" "$CURRENT_LINK"

if [[ -n "$LINK_BIN" ]]; then
  mkdir -p "$(dirname "$LINK_BIN")"
  ln -sfn "$CURRENT_LINK/bin/imparando" "$LINK_BIN"
fi

cat <<EOF
Installed bundle:
  release: $RELEASE_DIR
  current: $CURRENT_LINK
  binary:  ${LINK_BIN:-"(not linked)"}

Example run:
  sudo $CURRENT_LINK/bin/imparando --user yourname --pass yourpassword --data-dir $CURRENT_LINK/data
EOF
