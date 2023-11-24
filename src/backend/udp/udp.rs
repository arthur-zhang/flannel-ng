use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use async_trait::async_trait;
use etcd_client::EventType;
use ipnetwork::{IpNetwork, Ipv4Network};
use netlink_ng::types::Route;
use netlink_ng::{Addr, LinkId};
use nix::libc::RT_SCOPE_UNIVERSE;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

use crate::backend::common::{Backend, Network};
use crate::backend::udp::codec::IpSlice;
use crate::backend::ExternalInterface;
use crate::etcd::LeaseWatchEvent;
use crate::lease::{Lease, LeaseAttrs};
use crate::subnet::config::Config;
use crate::subnet::SubnetManager;
use crate::tun;
use crate::tun::Tun;

const DEFAULT_PORT: u16 = 8285;
const ENCAP_OVERHEAD: u32 = 28;

#[derive(Debug)]
pub struct UdpBackend {
    sm: SubnetManager,
    ext_iface: ExternalInterface,
}

#[async_trait]
impl Backend<UdpNetwork> for UdpBackend {
    async fn register_network(mut self, config: &Config) -> anyhow::Result<UdpNetwork> {
        let port = get_port_from_config(config);
        let attrs = LeaseAttrs {
            public_ip: self.ext_iface.iface_addr,
            ..Default::default()
        };
        let lease = self.sm.acquire_lease(&attrs).await?;

        // 172.17.40.0/24
        let net = config.get_flannel_network()?;

        let tun_net = Ipv4Network::new(lease.subnet.ip(), net.prefix())?;

        println!("lease: {:?}", lease);
        Ok(UdpNetwork::new(self.sm, self.ext_iface, port, tun_net, lease).await?)
    }
}

impl UdpBackend {
    pub fn new(sm: SubnetManager, ext_iface: ExternalInterface) -> Self {
        UdpBackend { sm, ext_iface }
    }
}

// ip_network: 172.17.43.1/16
fn configure_tun(ifname: &str, ip_network: &Ipv4Network, mtu: u32) -> anyhow::Result<()> {
    let link = netlink_ng::link_by_name(ifname)?;
    let link = link.ok_or(anyhow::anyhow!("link not found"))?;

    // Ensure that the device has a /32 address so that no broadcast routes are created.
    // This IP is just used as a source address for host to workload traffic (so
    // the return path for the traffic has an address on the flannel network to use as the destination)
    let ip_local = Ipv4Network::new(ip_network.ip(), 32)?;

    netlink_ng::addr_add(
        &link,
        &Addr {
            ipnet: IpNetwork::V4(ip_local),
            label: "".to_string(),
            ..Default::default()
        },
    )?;
    let link_id: LinkId = LinkId::from(&link);
    netlink_ng::link_set_mtu(link_id, mtu)?;
    netlink_ng::link_set_up(link_id)?;

    netlink_ng::route_add(&Route {
        link_index: link.attrs().index,
        scope: RT_SCOPE_UNIVERSE,
        dst: Some(IpNetwork::new(
            IpAddr::V4(ip_network.network()),
            ip_network.prefix(),
        )?),
        ..Default::default()
    })?;
    Ok(())
}

type TSubnetManager = Arc<SubnetManager>;

pub struct UdpNetwork {
    pub ext_iface: ExternalInterface,
    pub subnet_lease: Lease,
    pub port: u16,
    pub tun_net: Ipv4Network,
    pub sm: TSubnetManager,
    pub tun_file: Option<Tun>,
    pub conn: Option<UdpSocket>,
}

impl UdpNetwork {
    pub async fn new(
        sm: SubnetManager,
        ext_iface: ExternalInterface,
        port: u16,
        nw: Ipv4Network,
        lease: Lease,
    ) -> anyhow::Result<Self> {
        let mut n = UdpNetwork {
            ext_iface,
            subnet_lease: lease,
            port,
            tun_net: nw,
            sm: Arc::new(sm),
            tun_file: None,
            conn: None,
        };
        n.init_tun().await?;

        let addr = format!("{}:{}", n.ext_iface.iface_addr, n.port);
        let socket = UdpSocket::bind(addr).await?;
        n.conn = Some(socket);
        return Ok(n);
    }
    async fn init_tun(&mut self) -> anyhow::Result<()> {
        let (tun, name) = tun::open_tun("flannel%d").await?;
        self.tun_file = Some(Tun::new(tun)?);
        configure_tun(&name, &self.tun_net, self.mtu())?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct RouteEntry {
    // eg 172.17.5.0-24
    pub dst: Ipv4Network,
    // eg 198.19.249.111
    pub next_hop: Ipv4Addr,
}

impl PartialEq for RouteEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dst == other.dst
    }
}

#[async_trait]
impl Network for UdpNetwork {
    fn lease(&self) -> &Lease {
        &self.subnet_lease
    }

    fn mtu(&self) -> u32 {
        self.ext_iface.iface.mtu - ENCAP_OVERHEAD
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        let mut routes = vec![];
        let sock = self.conn.take().unwrap();
        let sock = Arc::new(sock);
        let mtu = self.mtu() as usize;

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Vec<LeaseWatchEvent>>(100);

        let mut tun_file = self.tun_file.take().unwrap();

        tokio::spawn({
            let sm = self.sm.clone();
            let lease = self.subnet_lease.clone();
            async move {
                let _ = sm.watch_leases(&lease, event_tx).await;
            }
        });

        let mut tun_buf = vec![0u8; mtu];
        let mut sock_buf = vec![0u8; mtu];
        loop {
            tokio::select! {
                Ok(n) = sock.recv(&mut sock_buf) => {
                    println!("read from sock: {:?}", n);
                    let mut ip_pkt = IpSlice::new(&mut sock_buf[..n])?;
                    decrement_ttl(&mut ip_pkt);
                    tun_file.write_all(&sock_buf[..n]).await?;
                }
                // tun to udp
                Ok(n) = tun_file.read(&mut tun_buf) => {
                    if n == 0 {
                        // should not happen
                        break;
                    }

                    let _ = send_udp_packet(&mut tun_buf[..n], &routes, sock.clone()).await;
                }
                Some(events) = event_rx.recv() => {
                    println!("event before[{}] {:?}", routes.len(), routes);

                    for e in events.into_iter() {
                        match e.event_type {
                            EventType::Put => {
                                for r in &mut routes {
                                    if r.dst == e.lease.subnet{
                                        r.next_hop = e.lease.attrs.public_ip;
                                        continue;
                                    }
                                }
                                let new_route = RouteEntry{dst: e.lease.subnet, next_hop: e.lease.attrs.public_ip};
                                routes.push(new_route);
                            }
                            EventType::Delete => {
                                routes.retain(|it| it.dst != e.lease.subnet);
                            }
                        }
                    }
                    println!("event after[{}] {:?}", routes.len(), routes);
                }
                else => {
                    println!("else......");
                    continue;
                }
            }
        }
        Ok(())
    }
}

async fn send_udp_packet(
    buf: &mut [u8],
    routes: &[RouteEntry],
    sock: Arc<UdpSocket>,
) -> anyhow::Result<()> {
    let mut ip_pkt = IpSlice::new(buf)?;
    if ip_pkt.version() != 4 {
        // handle ipv4 only
        return Ok(());
    }
    let dst_addr = Ipv4Addr::from(ip_pkt.destination());

    let route = find_route(&routes, dst_addr);
    match route {
        None => {
            println!("route not found");
        }
        Some(route) => {
            decrement_ttl(&mut ip_pkt);
            let addr = format!("{}:8285", route.next_hop);
            sock.send_to(&buf, &addr).await?;
        }
    }
    Ok(())
}

fn decrement_ttl(ip_pkt: &mut IpSlice) {
    let ttl = ip_pkt.ttl_mut();
    if *ttl > 0 {
        *ttl = *ttl - 1;
    }
}

// routes: [{172.17.43.0-24,198.19.249.136}, {172.17.5.0-24,198.19.249.111}]
// dst: 172.17.5.2
fn find_route(routes: &[RouteEntry], dst: Ipv4Addr) -> Option<&RouteEntry> {
    routes.iter().find(|it| {
        // 172.17.5.2/24
        let dst_network = Ipv4Network::new(dst, it.dst.prefix()).unwrap();
        // 172.17.5.0/25 == 172.17.43.0-24
        dst_network.network() == it.dst.ip()
    })
}

fn get_port_from_config(config: &Config) -> u16 {
    match &config.backend {
        None => DEFAULT_PORT,
        Some(backend) => backend
            .as_object()
            .and_then(|it| it.get("Port"))
            .and_then(|it| it.as_u64())
            .unwrap_or(DEFAULT_PORT as u64) as u16,
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    #[test]
    fn test_init() -> anyhow::Result<()> {
        let l = Lease {
            enable_ipv4: true,
            enable_ipv6: false,
            subnet: "172.17.43.1/24".parse().unwrap(),
            ..Default::default()
        };
        let net = "172.17.0.0/16".parse::<Ipv4Network>().unwrap();
        let tun_net = Ipv4Network::new(l.subnet.ip(), net.prefix())?;
        // let mut udp_backend = UdpBackend { tun_net, tun_file: None };
        // udp_backend.init_tun()?;

        sleep(std::time::Duration::from_secs(100000));
        Ok(())
    }
}
