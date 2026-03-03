use anyhow::Result;
use tokio::process::Command;

pub struct NetworkManager;

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
        // Bring up
        run("ip", &["link", "set", tap_name, "up"]).await?;
        // Enable IP forwarding
        run("sysctl", &["-w", "net.ipv4.ip_forward=1"]).await?;
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
        // Allow forwarding from tap to external
        let _ = run(
            "iptables",
            &[
                "-A", "FORWARD",
                "-i", tap_name,
                "-o", &ext_iface,
                "-j", "ACCEPT",
            ],
        )
        .await;
        // Only add the conntrack rule if not already present
        let check = run(
            "iptables",
            &[
                "-C", "FORWARD",
                "-m", "conntrack",
                "--ctstate", "RELATED,ESTABLISHED",
                "-j", "ACCEPT",
            ],
        )
        .await;
        if check.is_err() {
            let _ = run(
                "iptables",
                &[
                    "-A", "FORWARD",
                    "-m", "conntrack",
                    "--ctstate", "RELATED,ESTABLISHED",
                    "-j", "ACCEPT",
                ],
            )
            .await;
        }

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
                // Remove per-TAP FORWARD rule
                let _ = run(
                    "iptables",
                    &["-D", "FORWARD", "-i", tap_name, "-o", &ext_iface, "-j", "ACCEPT"],
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
