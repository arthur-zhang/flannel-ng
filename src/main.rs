use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use anyhow::bail;
use clap::Parser;
use netlink_ng::nl_type::FAMILY_V4;
use netlink_ng::LinkId;

use crate::backend::udp::UdpBackend;
use crate::backend::{Backend, ExternalInterface, Interface, Network};
use crate::etcd::{EtcdConfig, EtcdSubnetRegistry};
use crate::ip::get_default_gateway_interface;
use crate::subnet::SubnetManager;

mod backend;
mod etcd;
mod ip;
mod lease;
mod subnet;
mod tun;

#[derive(Parser, Debug)]
#[clap(author, about, long_about = None)]
struct Args {
    /// IP accessible by other nodes for inter-host communication
    #[clap(long, default_value = "")]
    public_ip: String,

    /// A comma-delimited list of etcd endpoints
    #[clap(long)]
    etcd_endpoints: String,

    /// etcd prefix
    #[clap(long, default_value = "/coreos.com/network")]
    etcd_prefix: String,

    /// SSL key file used to secure etcd communication
    #[clap(long)]
    etcd_keyfile: Option<String>,

    /// SSL certification file used to secure etcd communication
    #[clap(long)]
    etcd_certfile: Option<String>,

    /// SSL Certificate Authority file used to secure etcd communication
    #[clap(long)]
    etcd_cafile: Option<String>,

    /// Contact the Kubernetes API for subnet assignment instead of etcd
    #[clap(long)]
    kube_subnet_mgr: bool,

    /// Interface to use for inter-host communication
    #[clap(long, default_value = "")]
    iface: String,

    /// Regex expression to match the first interface for inter-host communication
    #[clap(long, default_value = "")]
    iface_regex: String,

    /// Detect interface to use for inter-host communication based on provided IP
    #[clap(long)]
    iface_can_reach: Option<String>,

    /// Resync period for iptables rules, in seconds
    #[clap(long, default_value_t = 5)]
    iptables_resync: u32,

    /// Filename where env variables (subnet and MTU values) will be written to
    #[clap(long, default_value = "/run/flannel/subnet.env")]
    subnet_file: String,

    /// Path to the network configuration file to use
    #[clap(long, default_value = "/etc/kube-flannel/net-conf.json")]
    net_config_path: String,

    /// Subnet lease renewal margin, in minutes
    #[clap(long, default_value_t = 60)]
    subnet_lease_renew_margin: u32,

    /// Setup IP masquerade for traffic destined for outside the flannel network
    #[clap(long)]
    ip_masq: bool,

    /// Log level for V logs
    #[clap(short, long, default_value_t = 0)]
    v: u32,

    /// The IP address for healthz server to listen
    #[clap(long, default_value = "0.0.0.0")]
    healthz_ip: String,

    /// The port for healthz server to listen(0 to disable)
    #[clap(long, default_value_t = 0)]
    healthz_port: u16,

    /// Print version and exit
    #[clap(long)]
    version: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    println!("args: {:?}", args);
    let cfg = EtcdConfig {
        endpoints: args
            .etcd_endpoints
            .split(',')
            .map(|it| it.to_string())
            .collect::<Vec<_>>(),
        prefix: args.etcd_prefix.clone(),
        ..Default::default()
    };
    println!("etcd cfg: {:?}", cfg);
    let ext_iface = lookup_ext_iface()?;
    println!("ext iface: {:?}", cfg);
    let subnet_file_map = read_from_subnet_env(Path::new(&args.subnet_file))?;
    println!("subnet_file_map: {:?}", subnet_file_map);
    let prev_subnet = subnet_file_map
        .get("FLANNEL_SUBNET")
        .map(|it| it.parse())
        .transpose()?;

    let mut sm = SubnetManager::new(
        EtcdSubnetRegistry::new(cfg).await?,
        prev_subnet,
        args.subnet_lease_renew_margin,
    );
    let config = sm.get_network_config().await?;
    let backend = UdpBackend::new(sm.clone(), ext_iface);
    let mut bn = backend.register_network(&config).await?;

    SubnetManager::write_subnet_file(
        Path::new(&args.subnet_file),
        &config,
        args.ip_masq,
        bn.lease().subnet,
        bn.mtu(),
    )?;
    let mut lease = bn.lease().clone();
    tokio::spawn(async move {
        let res = bn.run().await;
        println!("bn run exit!, res is: {:?}", res);
    });

    let resp = sm.renew_lease_if_needed(&mut lease).await;
    println!("renew lease quit, resp is: {:?}", resp);
    Ok(())
}

fn get_iface_addrs(ext_iface: &Interface) -> anyhow::Result<Vec<Ipv4Addr>> {
    let addr_list = netlink_ng::addr_list(LinkId::Id(ext_iface.index), FAMILY_V4)?;
    let mut addrs = vec![];
    let mut ll = vec![];
    for addr in addr_list {
        if !addr.ipnet.is_ipv4() {
            continue;
        }
        let ip = match addr.ipnet.ip() {
            IpAddr::V4(addr) => addr,
            IpAddr::V6(_) => {
                unreachable!("ipv6")
            }
        };

        if ip != Ipv4Addr::BROADCAST
            && !ip.is_unspecified()
            && !ip.is_loopback()
            && !ip.is_multicast()
            && !ip.is_link_local()
        {
            addrs.push(ip);
            continue;
        }
        if ip.is_link_local() {
            ll.push(ip);
        }
    }
    if !ll.is_empty() {
        addrs.extend(ll);
    }
    if addrs.is_empty() {
        bail!(
            "Could not find any valid address on interface {}",
            ext_iface.name
        );
    }

    Ok(addrs)
}

fn lookup_ext_iface() -> anyhow::Result<ExternalInterface> {
    let ext_iface = get_default_gateway_interface()?;

    if ext_iface.is_none() {
        bail!("Could not find default gateway interface");
    }
    let ext_iface = ext_iface.unwrap();

    let mut addrs = get_iface_addrs(&ext_iface)?;

    let iface_addr = addrs.pop().unwrap();
    return Ok(ExternalInterface {
        iface: ext_iface,
        iface_addr,
        iface_v6_addr: None,
        ext_addr: Some(iface_addr),
        ext_v6_addr: None,
    });
}

fn read_from_subnet_env(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let mut f = File::open(path)?;

    let mut s = String::new();
    let n = f.read_to_string(&mut s)?;
    if n == 0 {
        return Ok(HashMap::new());
    }
    Ok(s.lines()
        .filter(|it| !it.is_empty())
        .filter_map(|it| it.trim().split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<_, _>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_ext_iface() {
        let ext_iface = lookup_ext_iface().unwrap();
        println!("{:?}", ext_iface);
    }
}
