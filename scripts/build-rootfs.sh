#!/bin/bash
set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
success() { echo -e "${GREEN}[OK]${NC} $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*"; }

# Check running as root
if [ "$(id -u)" -ne 0 ]; then
    error "This script must be run as root."
    exit 1
fi
success "Running as root."

# Check debootstrap is installed
if ! command -v debootstrap &>/dev/null; then
    error "debootstrap is not installed. Install it with: apt-get install debootstrap"
    exit 1
fi
success "debootstrap is available."

# Create output directory
mkdir -p /var/lib/imparando
info "Ensured /var/lib/imparando exists."

# Check if base.ext4 already exists
if [ -f /var/lib/imparando/base.ext4 ]; then
    warn "base.ext4 already exists at /var/lib/imparando/base.ext4. Remove it to rebuild."
    exit 0
fi

# Create temporary chroot and mount directories under /var/lib/imparando
# rather than /tmp, because some hosts mount /tmp with noexec.
TMPROOT=/var/lib/imparando/tmp
mkdir -p "$TMPROOT"
TMPDIR=$(mktemp -d "$TMPROOT"/imparando-rootfs-XXXXXX)
MOUNTDIR=$(mktemp -d "$TMPROOT"/imparando-mount-XXXXXX)
info "Created temporary chroot at $TMPDIR"

cleanup() {
  for mp in "$TMPDIR/dev/pts" "$TMPDIR/dev" "$TMPDIR/sys" "$TMPDIR/proc"; do
    if mountpoint -q "$mp" 2>/dev/null; then
      umount "$mp" || true
    fi
  done
  # Unmount if still mounted
  if mountpoint -q "$MOUNTDIR" 2>/dev/null; then
    umount "$MOUNTDIR" || true
  fi
  # Remove temp dirs
  rm -rf "$TMPDIR"
  rm -rf "$MOUNTDIR"
}
trap cleanup EXIT

mount_chroot_fs() {
  mount -t proc proc "$TMPDIR/proc"
  mount -t sysfs sysfs "$TMPDIR/sys"
  mount --bind /dev "$TMPDIR/dev"
  mount -t devpts devpts "$TMPDIR/dev/pts"
}

unmount_chroot_fs() {
  for mp in "$TMPDIR/dev/pts" "$TMPDIR/dev" "$TMPDIR/sys" "$TMPDIR/proc"; do
    if mountpoint -q "$mp" 2>/dev/null; then
      umount "$mp"
    fi
  done
}

# Run debootstrap for Ubuntu jammy
info "Running debootstrap for Ubuntu 22.04 (jammy)..."
debootstrap --components=main,universe \
    --include=curl,ca-certificates,ethtool,git,haveged,openssh-client,iproute2,iptables \
    jammy "$TMPDIR" http://archive.ubuntu.com/ubuntu/
success "debootstrap complete."

info "Configuring Ubuntu APT sources..."
cat > "$TMPDIR/etc/apt/sources.list" <<'EOF'
deb http://archive.ubuntu.com/ubuntu jammy main universe
deb http://archive.ubuntu.com/ubuntu jammy-updates main universe
deb http://security.ubuntu.com/ubuntu jammy-security main universe
EOF
success "Ubuntu APT sources configured."

info "Mounting chroot pseudo-filesystems..."
mount_chroot_fs
success "Chroot pseudo-filesystems mounted."

# Install Node.js LTS via NodeSource
info "Installing Node.js 20.x via NodeSource..."
curl -fsSL https://deb.nodesource.com/setup_20.x -o /tmp/nodesource_setup.sh
chroot "$TMPDIR" bash -c "apt-get install -y curl ca-certificates"
cp /tmp/nodesource_setup.sh "$TMPDIR/tmp/nodesource_setup.sh"
chroot "$TMPDIR" bash /tmp/nodesource_setup.sh
chroot "$TMPDIR" apt-get install -y nodejs
success "Node.js installed."

# Install Claude Code globally
info "Installing @anthropic-ai/claude-code globally..."
chroot "$TMPDIR" npm install -g @anthropic-ai/claude-code
success "Claude Code installed."

# Install Codex CLI globally
info "Installing @openai/codex globally..."
chroot "$TMPDIR" npm install -g @openai/codex
success "Codex installed."

# Enable Corepack so pnpm/yarn are available without separate npm installs.
info "Enabling Corepack..."
chroot "$TMPDIR" corepack enable
success "Corepack enabled."

# Install general development tooling commonly needed by agents.
info "Installing general development tooling..."
chroot "$TMPDIR" apt-get install -y \
  build-essential \
  pkg-config \
  libssl-dev \
  libsqlite3-dev \
  python3-pip \
  python3-venv \
  jq \
  ripgrep \
  fd-find \
  unzip \
  zip \
  xz-utils
success "General development tooling installed."

# Add the official PostgreSQL APT repository so Ubuntu 22.04 can install
# PostgreSQL 17 instead of the distro-default PostgreSQL 14.
info "Configuring PostgreSQL APT repository..."
chroot "$TMPDIR" bash -lc '
set -e
install -d /usr/share/postgresql-common/pgdg
curl -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc --fail https://www.postgresql.org/media/keys/ACCC4CF8.asc
. /etc/os-release
echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt ${VERSION_CODENAME}-pgdg main" > /etc/apt/sources.list.d/pgdg.list
apt-get update
'
success "PostgreSQL APT repository configured."

# Install the latest stable Rust toolchain with rustup and expose it system-wide.
info "Installing latest stable Rust via rustup..."
chroot "$TMPDIR" bash -lc '
set -e
curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain stable --profile default
cat > /etc/profile.d/rust.sh <<EOF
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo
export PATH=/root/.cargo/bin:\$PATH
EOF
ln -sf /root/.cargo/bin/cargo /usr/local/bin/cargo
ln -sf /root/.cargo/bin/rustc /usr/local/bin/rustc
ln -sf /root/.cargo/bin/rustfmt /usr/local/bin/rustfmt
ln -sf /root/.cargo/bin/rustup /usr/local/bin/rustup
ln -sf /root/.cargo/bin/clippy-driver /usr/local/bin/clippy-driver
/root/.cargo/bin/cargo install just --locked
/root/.cargo/bin/cargo install cargo-llvm-cov --locked
/root/.cargo/bin/cargo install sqlx-cli --no-default-features --features rustls,postgres --locked
ln -sf /root/.cargo/bin/just /usr/local/bin/just
ln -sf /root/.cargo/bin/cargo-llvm-cov /usr/local/bin/cargo-llvm-cov
ln -sf /root/.cargo/bin/sqlx /usr/local/bin/sqlx
'
success "Rust installed."

# Prevent postgresql-common from trying to create a default cluster during
# package installation; we create it explicitly below without relying on su.
mkdir -p "$TMPDIR/etc/postgresql-common"
cat > "$TMPDIR/etc/postgresql-common/createcluster.conf" << 'PGCONF'
create_main_cluster = false
PGCONF
chroot "$TMPDIR" bash -lc "printf 'postgresql-common postgresql-common/createcluster boolean false\n' | debconf-set-selections"

# Install tmux (persistent terminal sessions) and PostgreSQL
info "Installing tmux and PostgreSQL..."
chroot "$TMPDIR" apt-get install -y \
  -o Dpkg::Options::=--force-confdef \
  -o Dpkg::Options::=--force-confold \
  tmux postgresql-17 postgresql-client-17
success "tmux and PostgreSQL installed."

# Configure PostgreSQL to start without systemd
info "Configuring PostgreSQL for microVM use..."
chroot "$TMPDIR" bash -lc '
set -e
if [ -d /usr/lib/postgresql/17 ]; then
  PG_VERSION=17
else
  PG_VERSION=$(ls /usr/lib/postgresql | sort -V | tail -1)
fi
mkdir -p /var/lib/postgresql/$PG_VERSION/main
chown -R postgres:postgres /var/lib/postgresql
mkdir -p /etc/postgresql/$PG_VERSION/main
cat > /etc/postgresql/$PG_VERSION/main/start.conf <<EOF
auto
EOF
'
cat > "$TMPDIR/usr/local/bin/start-postgres.sh" << 'PGEOF'
#!/bin/bash
# Start PostgreSQL in the microVM (no systemd)
set -euo pipefail
if [ -d /usr/lib/postgresql/17 ]; then
  PG_VERSION=17
else
  PG_VERSION=$(ls /usr/lib/postgresql | sort -V | tail -1)
fi
PG_DATA="/var/lib/postgresql/$PG_VERSION/main"
PG_BIN="/usr/lib/postgresql/$PG_VERSION/bin"
RUN_AS_POSTGRES=(chroot --userspec=postgres:postgres /)

# Fix ownership (may be wrong after overlay copy)
mkdir -p /var/run/postgresql /var/log/postgresql
chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql

# Initialize the cluster on first boot inside the VM, where switching to the
# postgres user no longer depends on setuid su.
if [ ! -s "$PG_DATA/PG_VERSION" ]; then
  rm -rf "$PG_DATA"
  mkdir -p "$PG_DATA"
  chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql
  "${RUN_AS_POSTGRES[@]}" "$PG_BIN/initdb" -D "$PG_DATA"
fi

chmod 700 "$PG_DATA"
sed -i "s/^#listen_addresses = .*/listen_addresses = '127.0.0.1'/" "$PG_DATA/postgresql.conf"
if ! grep -q "^unix_socket_directories = '/var/run/postgresql'" "$PG_DATA/postgresql.conf"; then
  printf "\nunix_socket_directories = '/var/run/postgresql'\n" >> "$PG_DATA/postgresql.conf"
fi

# Start PostgreSQL
"${RUN_AS_POSTGRES[@]}" "$PG_BIN/pg_ctl" -D "$PG_DATA" -l /var/log/postgresql/postgresql.log start -w
echo "PostgreSQL started on port 5432"
PGEOF
chmod +x "$TMPDIR/usr/local/bin/start-postgres.sh"
success "PostgreSQL configured."

# Write /startup.sh placeholder
info "Writing /startup.sh placeholder..."
cat > "$TMPDIR/startup.sh" << 'INNEREOF'
#!/bin/bash
echo "No startup script found" > /dev/ttyS0
sleep infinity
INNEREOF
chmod +x "$TMPDIR/startup.sh"
success "/startup.sh placeholder written."

# Write /etc/fstab
info "Writing /etc/fstab..."
printf "proc /proc proc defaults 0 0\nsysfs /sys sysfs defaults 0 0\n" > "$TMPDIR/etc/fstab"
success "/etc/fstab written."

info "Unmounting chroot pseudo-filesystems before packaging..."
unmount_chroot_fs
success "Chroot pseudo-filesystems unmounted."

# Pack chroot into ext4 image
SIZE=51200
info "Creating ext4 image of ${SIZE}MB at /var/lib/imparando/base.ext4..."
truncate -s "${SIZE}M" /var/lib/imparando/base.ext4
mkfs.ext4 /var/lib/imparando/base.ext4
success "ext4 filesystem created."

info "Mounting ext4 image at $MOUNTDIR..."
mount -o loop /var/lib/imparando/base.ext4 "$MOUNTDIR"

info "Copying chroot contents into image with rsync..."
# Avoid preserving host ACLs/xattrs into the guest image. They are not needed
# here and can cause non-root exec/linker permission failures inside the VM.
rsync -aH --info=progress2 \
  --exclude=/proc/* \
  --exclude=/sys/* \
  --exclude=/dev/* \
  "$TMPDIR/" "$MOUNTDIR/"

info "Unmounting image..."
umount "$MOUNTDIR"
rmdir "$MOUNTDIR"
success "Image populated and unmounted."

success "Root filesystem image created successfully at /var/lib/imparando/base.ext4"
