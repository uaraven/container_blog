use std::{net::Ipv4Addr, process::Command};

use anyhow::Context;
use nix::unistd::Pid;

use cidr::Ipv4Cidr;

const BRIDGE_NAME: &str = "br0";
const VETH_HOST: &str = "veth0h0";
const VETH_CONTAINER: &str = "veth0c0";

/// executes ip command with arguments
fn ip(args: &[&str]) -> anyhow::Result<()> {
    Command::new("/sbin/ip")
        .args(args)
        .status()
        .context(format!("Failed to execute ip {:?}", args))?;
    Ok(())
}

fn ips_from_cidr(netw: &Ipv4Cidr) -> anyhow::Result<(Ipv4Addr, Ipv4Addr)> {
    let host_ip = netw
        .iter()
        .nth(1)
        .context("get host address from cidr")?
        .address();
    let container_ip = netw
        .iter()
        .nth(2)
        .context("get container address from cidr")?
        .address();
    Ok((host_ip, container_ip))
}

/// creates a bridge with the given IP address and brings the interface up
fn create_bridge(ipaddr: &Ipv4Addr) -> anyhow::Result<()> {
    ip(&["link", "add", "name", BRIDGE_NAME, "type", "bridge"]).context("creating bridge")?;
    ip(&[
        "addr",
        "add",
        format!("{}/24", ipaddr).as_str(),
        "dev",
        BRIDGE_NAME,
    ])
    .context("adding IP address to bridge")?;
    ip(&["link", "set", "dev", BRIDGE_NAME, "up"]).context("bringing up bridge")?;
    Ok(())
}

/// creates a veth pair with the given container IP address and brings the interface up
fn create_veth_pair() -> anyhow::Result<()> {
    // create veth pair
    ip(&[
        "link",
        "add",
        "name",
        VETH_HOST,
        "type",
        "veth",
        "peer",
        "name",
        VETH_CONTAINER,
    ])
    .context("creating veth pair")?;

    // bring host side up
    ip(&["link", "set", "dev", VETH_HOST, "up"]).context("bringing up host side")?;

    // attach host veth side to the bridge interface
    ip(&["link", "set", "dev", VETH_HOST, "master", BRIDGE_NAME])
        .context("attaching host side to the bridge")?;

    Ok(())
}

pub(crate) fn move_into_container(child_pid: Pid) -> anyhow::Result<()> {
    // move child side to child namespace
    let pid_s: String = child_pid.to_string();
    ip(&[
        "link",
        "set",
        "dev",
        VETH_CONTAINER,
        "netns",
        pid_s.as_str(),
    ])
    .context("moving veth0c0 to child namespace")?;

    Ok(())
}

/// setup the network on the host side:
/// - create bridge and assign first address in the CIDR to the bridge interface
/// - attach host veth side to the bridge interface
/// - assign IP address to container veth side
/// - move container veth side into container namespace
pub(crate) fn setup_network_host(netw: &Ipv4Cidr) -> anyhow::Result<()> {
    let (host_ip, _) = ips_from_cidr(netw)?;

    create_bridge(&host_ip)?;
    create_veth_pair()?;

    Ok(())
}

/// bring up the network on the container side:
/// - bring up the container veth side, if the `veth` parameter is true
/// - bring up the loopback interface
pub(crate) fn bring_up_container_net(netw: &Ipv4Cidr, is_root: bool) -> anyhow::Result<()> {
    let (host_ip, container_ip) = ips_from_cidr(netw)?;

    if is_root {
        // assign IP address to container veth side
        ip(&[
            "addr",
            "add",
            format!("{}/24", container_ip).as_str(),
            "dev",
            VETH_CONTAINER,
        ])
        .context("adding IP address to container veth")?;

        // bring container side up
        ip(&["link", "set", "dev", VETH_CONTAINER, "up"])
            .context("bringing up container veth side")?;

        // configure default gateway
        ip(&[
            "route",
            "add",
            "default",
            "via",
            host_ip.to_string().as_str(),
            "dev",
            VETH_CONTAINER,
        ])
        .context("configure default route")?;
    }
    ip(&["link", "set", "dev", "lo", "up"]).context("bringing up lo in container")?;

    Ok(())
}

pub(crate) fn cleanup_network() -> anyhow::Result<()> {
    ip(&["link", "delete", BRIDGE_NAME]).context("removing bridge device")?;

    Ok(())
}
