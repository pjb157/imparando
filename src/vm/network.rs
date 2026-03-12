use anyhow::Result;
use tokio::process::Command;

pub struct NetworkManager;

const VM_MTU: &str = "576";
const VM_ADVMSS: &str = "536";

impl NetworkManager {
    /// Create a TAP device and assign the given CIDR IP to it.
    /// Also enables IP forwarding and sets up iptables masquerade.
    pub async fn setup_tap(tap_name: &str, tap_cidr: &str) -> Result<()> {
        // Remove stale TAP device if it already exists (e.g. from a crashed session).
        let _ = run("ip", &["link", "del", tap_name]).await;
        // Create TAP device
        run("ip", &["tuntap", "add", tap_name, "mode", "tap"]).await?;
        // Assign IP
        run("ip", &["addr", "add", tap_cidr, "dev", tap_name]).await?;
        // Bring up with a conservative MTU to force small TCP segments even
        // when the guest NIC ignores offload feature changes.
        run("ip", &["link", "set", tap_name, "up"]).await?;
        run("ip", &["link", "set", tap_name, "mtu", VM_MTU]).await?;
        // Disable TX checksum/segmentation offloading on TAP device.
        // Without this, locally-originated TCP to the VM may have bad checksums
        // because the kernel expects a NIC to compute them, but TAP has no NIC.
        if run("ethtool", &["-K", tap_name, "tx", "off", "sg", "off", "tso", "off", "gso", "off", "gro", "off"]).await.is_err() {
            tracing::warn!("ethtool not found or failed — install ethtool for reliable VM networking: apt-get install ethtool");
        }
        // Enable IP forwarding
        run("sysctl", &["-w", "net.ipv4.ip_forward=1"]).await?;
        // Disable conntrack checksum verification — virtio-net uses checksum
        // offloading, so packets from VMs may have partial/no checksums.
        // With this enabled, conntrack marks them INVALID and they get dropped.
        let _ = run("sysctl", &["-w", "net.netfilter.nf_conntrack_checksum=0"]).await;
        // NAT masquerade for this tap's subnet (derive network from CIDR)
        let network = cidr_to_network(tap_cidr);
        let ext_iface = detect_external_iface().await.unwrap_or_else(|| "eth0".to_string());
        // Only add masquerade if not already present
        let _ = run(
            "iptables",
            &[
                "-t", "nat", "-A", "POSTROUTING",
                "-s", &network,
                "-o", &ext_iface,
                "-j", "MASQUERADE",
            ],
        )
        .await;
        // Allow ALL forwarded traffic to/from the tap device.
        // Use -I (insert at top) so rules take precedence over k8s/Calico.
        let _ = run(
            "iptables",
            &["-I", "FORWARD", "1", "-i", tap_name, "-j", "ACCEPT"],
        )
        .await;
        let _ = run(
            "iptables",
            &["-I", "FORWARD", "1", "-o", tap_name, "-j", "ACCEPT"],
        )
        .await;
        // Allow ALL input/output traffic from/to the tap device.
        // Calico/k8s may have INPUT rules that drop VM traffic.
        let _ = run(
            "iptables",
            &["-I", "INPUT", "1", "-i", tap_name, "-j", "ACCEPT"],
        )
        .await;
        let _ = run(
            "iptables",
            &["-I", "OUTPUT", "1", "-o", tap_name, "-j", "ACCEPT"],
        )
        .await;
        // Clamp TCP MSS to path MTU — prevents TLS hangs when the host
        // network has a reduced MTU (e.g. k8s overlay, Tailscale, VPN).
        if let Err(e) = run(
            "iptables",
            &[
                "-t", "mangle",
                "-I", "FORWARD", "1",
                "-p", "tcp",
                "--tcp-flags", "SYN,RST", "SYN",
                "-j", "TCPMSS",
                "--clamp-mss-to-pmtu",
            ],
        )
        .await {
            tracing::warn!(error = %e, "Failed to add MSS clamp rule (mangle table)");
        }
        // Also try via nft directly in case iptables-nft doesn't work for mangle
        let _ = run(
            "nft",
            &[
                "add", "rule", "ip", "filter", "FORWARD",
                "oifname", tap_name,
                "tcp", "flags", "syn",
                "tcp", "option", "maxseg", "size", "set", VM_ADVMSS,
            ],
        )
        .await;

        tracing::info!(tap = tap_name, ip = tap_cidr, "TAP device created");
        Ok(())
    }

    /// Remove a TAP device and clean up iptables rules.
    pub async fn teardown_tap(tap_name: &str) -> Result<()> {
        let _ = run("ip", &["link", "del", tap_name]).await;

        // Reconstruct the CIDR from tap name (tap{n} → 172.16.{n}.1/24)
        if let Some(index_str) = tap_name.strip_prefix("tap") {
            if let Ok(index) = index_str.parse::<u8>() {
                let network = format!("172.16.{index}.0/24");
                let ext_iface = detect_external_iface().await.unwrap_or_else(|| "eth0".to_string());
                // Remove MASQUERADE rule
                let _ = run(
                    "iptables",
                    &["-t", "nat", "-D", "POSTROUTING", "-s", &network, "-o", &ext_iface, "-j", "MASQUERADE"],
                ).await;
                // Remove per-TAP FORWARD rules
                let _ = run(
                    "iptables",
                    &["-D", "FORWARD", "-i", tap_name, "-j", "ACCEPT"],
                ).await;
                let _ = run(
                    "iptables",
                    &["-D", "FORWARD", "-o", tap_name, "-j", "ACCEPT"],
                ).await;
                // Remove per-TAP INPUT/OUTPUT rules
                let _ = run(
                    "iptables",
                    &["-D", "INPUT", "-i", tap_name, "-j", "ACCEPT"],
                ).await;
                let _ = run(
                    "iptables",
                    &["-D", "OUTPUT", "-o", tap_name, "-j", "ACCEPT"],
                ).await;
            }
        }

        tracing::info!(tap = tap_name, "TAP device removed");
        Ok(())
    }
}

async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd).args(args).status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Command '{cmd} {}' failed with exit code {:?}",
            args.join(" "),
            status.code()
        ))
    }
}

/// Convert a CIDR like "172.16.1.1/24" to a network address "172.16.1.0/24".
fn cidr_to_network(cidr: &str) -> String {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return cidr.to_string();
    }
    let ip = parts[0];
    let prefix: u8 = parts[1].parse().unwrap_or(24);
    let octets: Vec<u8> = ip.split('.').filter_map(|o| o.parse().ok()).collect();
    if octets.len() != 4 {
        return cidr.to_string();
    }
    let ip_u32 = u32::from_be_bytes([octets[0], octets[1], octets[2], octets[3]]);
    let mask = if prefix == 0 { 0 } else { !((1u32 << (32 - prefix)) - 1) };
    let net = ip_u32 & mask;
    let [a, b, c, d] = net.to_be_bytes();
    format!("{a}.{b}.{c}.{d}/{prefix}")
}

/// Detect the default external network interface.
async fn detect_external_iface() -> Option<String> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .await
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // "default via 1.2.3.4 dev eth0 ..."
    stdout
        .split_whitespace()
        .skip_while(|&w| w != "dev")
        .nth(1)
        .map(|s| s.to_string())
}
