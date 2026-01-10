use std::{net::Ipv4Addr, process::Command};

use anyhow::Context;
use nix::unistd::Pid;

use cidr::Ipv4Cidr;

const BRIDGE_NAME: &str = "br0";
const VETH_HOST: &str = "veth0h0";
const VETH_CONTAINER: &str = "veth0c0";

/// executes ip command with arguments
fn ip(args: &[&str]) -> anyhow::Result<()> {
    Command::new("ip")
        .args(args)
        .output()
        .context(format!("Failed to execute ip {:?}", args))?;
    Ok(())
}

/// creates a bridge with the given IP address and brings the interface up
fn create_bridge(ipaddr: &Ipv4Addr) -> anyhow::Result<()> {
    ip(&["link", "add", "name", BRIDGE_NAME, "type", "bridge"]).context("creating bridge")?;
    ip(&[
        "addr",
        "add",
        ipaddr.to_string().as_str(),
        "dev",
        BRIDGE_NAME,
    ])
    .context("adding IP address to bridge")?;
    ip(&["link", "set", "dev", BRIDGE_NAME, "up"]).context("bringing up bridge")?;
    Ok(())
}

/// creates a veth pair with the given container IP address and brings the interface up
fn create_veth_pair(container_ip: &Ipv4Addr) -> anyhow::Result<()> {
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
    ip(&["link", "set", VETH_HOST, "up"]).context("bringing up host side")?;

    ip(&["link", "set", VETH_HOST, "master", BRIDGE_NAME])
        .context("attaching host side to the bridge")?;

    ip(&[
        "addr",
        "add",
        container_ip.to_string().as_str(),
        "dev",
        VETH_CONTAINER,
    ])
    .context("adding IP address to bridge")?;

    Ok(())
}

pub(crate) fn setup_network(netw: &Ipv4Cidr, child_pid: Pid) -> anyhow::Result<()> {
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

    create_bridge(&host_ip)?;
    create_veth_pair(&container_ip)?;

    // move child side to child namespace
    let pid_s: String = child_pid.to_string();
    ip(&["link", "set", "dev", "veth0c0", "netns", pid_s.as_str()])
        .context("moving veth0c0 to child namespace")?;

    Ok(())
}
