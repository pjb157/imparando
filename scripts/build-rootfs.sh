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

# Create temporary chroot and mount directories
TMPDIR=$(mktemp -d /tmp/imparando-rootfs-XXXXXX)
MOUNTDIR=$(mktemp -d /tmp/imparando-mount-XXXXXX)
info "Created temporary chroot at $TMPDIR"

cleanup() {
  # Unmount if still mounted
  if mountpoint -q "$MOUNTDIR" 2>/dev/null; then
    umount "$MOUNTDIR" || true
  fi
  # Remove temp dirs
  rm -rf "$TMPDIR"
  rm -rf "$MOUNTDIR"
}
trap cleanup EXIT

# Run debootstrap for Ubuntu jammy
info "Running debootstrap for Ubuntu 22.04 (jammy)..."
debootstrap --include=curl,ca-certificates,git,openssh-client,iproute2,iptables \
    jammy "$TMPDIR" http://archive.ubuntu.com/ubuntu/
success "debootstrap complete."

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

# Pack chroot into ext4 image
SIZE=4096
info "Creating ext4 image of ${SIZE}MB at /var/lib/imparando/base.ext4..."
truncate -s "${SIZE}M" /var/lib/imparando/base.ext4
mkfs.ext4 /var/lib/imparando/base.ext4
success "ext4 filesystem created."

info "Mounting ext4 image at $MOUNTDIR..."
mount -o loop /var/lib/imparando/base.ext4 "$MOUNTDIR"

info "Copying chroot contents into image with rsync..."
rsync -aHAX --info=progress2 "$TMPDIR/" "$MOUNTDIR/"

info "Unmounting image..."
umount "$MOUNTDIR"
rmdir "$MOUNTDIR"
success "Image populated and unmounted."

success "Root filesystem image created successfully at /var/lib/imparando/base.ext4"
