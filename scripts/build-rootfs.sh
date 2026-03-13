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
  tmux postgresql postgresql-client
success "tmux and PostgreSQL installed."

# Configure PostgreSQL to start without systemd
info "Configuring PostgreSQL for microVM use..."
chroot "$TMPDIR" bash -lc '
set -e
PG_VERSION=$(ls /usr/lib/postgresql | sort -V | tail -1)
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
set -e
PG_VERSION=$(ls /usr/lib/postgresql | sort -V | tail -1)
PG_DATA="/var/lib/postgresql/$PG_VERSION/main"
PG_ETC="/etc/postgresql/$PG_VERSION/main"

# Fix ownership (may be wrong after overlay copy)
mkdir -p /var/run/postgresql /var/log/postgresql
chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql

# Initialize the cluster on first boot inside the VM, where switching to the
# postgres user works normally.
if [ ! -s "$PG_DATA/PG_VERSION" ]; then
  rm -rf "$PG_DATA"
  mkdir -p "$PG_DATA" "$PG_ETC"
  chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql
  su -s /bin/sh postgres -c "/usr/lib/postgresql/$PG_VERSION/bin/initdb -D '$PG_DATA'"
  cp "$PG_DATA/postgresql.conf" "$PG_ETC/postgresql.conf"
  cp "$PG_DATA/pg_hba.conf" "$PG_ETC/pg_hba.conf"
  cp "$PG_DATA/pg_ident.conf" "$PG_ETC/pg_ident.conf"
fi

chmod 700 "$PG_DATA"
sed -i "s/^#listen_addresses = .*/listen_addresses = '127.0.0.1'/" "$PG_ETC/postgresql.conf"

# Start PostgreSQL (use su -s to avoid login shell cd issues)
su -s /bin/sh postgres -c "/usr/lib/postgresql/$PG_VERSION/bin/pg_ctl -D '$PG_DATA' -l /var/log/postgresql/postgresql.log start -w"
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
SIZE=4096
info "Creating ext4 image of ${SIZE}MB at /var/lib/imparando/base.ext4..."
truncate -s "${SIZE}M" /var/lib/imparando/base.ext4
mkfs.ext4 /var/lib/imparando/base.ext4
success "ext4 filesystem created."

info "Mounting ext4 image at $MOUNTDIR..."
mount -o loop /var/lib/imparando/base.ext4 "$MOUNTDIR"

info "Copying chroot contents into image with rsync..."
rsync -aHAX --info=progress2 \
  --exclude=/proc/* \
  --exclude=/sys/* \
  --exclude=/dev/* \
  "$TMPDIR/" "$MOUNTDIR/"

info "Unmounting image..."
umount "$MOUNTDIR"
rmdir "$MOUNTDIR"
success "Image populated and unmounted."

success "Root filesystem image created successfully at /var/lib/imparando/base.ext4"
