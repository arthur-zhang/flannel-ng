use std::net::{IpAddr, Ipv4Addr};

use anyhow::Context;
use ipnetwork::Ipv4Network;
use macaddr::MacAddr6;
use netlink_ng::{Link, LinkAttrs, LinkKind, Neigh, unwrap_enum};
use netlink_ng::nl_type::Vxlan;

use crate::{ip, mac};
use crate::prelude::*;

#[derive(Clone, Debug)]
pub struct VxlanDeviceAttrs {
    pub vni: u32,
    pub name: String,
    pub mtu: u32,
    pub vtep_index: u32,
    pub vtep_addr: IpAddr,
    pub vtep_port: u16,
    pub gbp: bool,
    // pub learning: bool,
}

#[derive(Debug)]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    pub mac: MacAddr6,
}

#[derive(Debug)]
pub struct VxlanDevice {
    pub link: Link,
    pub direct_routing: bool,
}

impl VxlanDevice {
    pub fn new(vxlan_device_attrs: VxlanDeviceAttrs, direct_routing: bool) -> anyhow::Result<Self> {
        let mac_addr = mac::new_hardware_addr()?;

        let link = Link {
            link_attrs: LinkAttrs {
                name: vxlan_device_attrs.name,
                mtu: vxlan_device_attrs.mtu - 50,
                hardware_addr: Some(mac_addr),
                ..Default::default()
            },
            link_kind: LinkKind::Vxlan(Vxlan {
                vxlan_id: vxlan_device_attrs.vni,
                vtep_dev_index: vxlan_device_attrs.vtep_index,
                src_addr: Some(vxlan_device_attrs.vtep_addr),
                gbp: vxlan_device_attrs.gbp,
                ..Default::default()
            }),
        };

        let vxlan_link = ensure_vxlan_link(&link).context("ensure_vxlan_link error")?;

        Ok(Self { link: vxlan_link, direct_routing })
    }

    pub fn configure(&self, ipa: Ipv4Network, flannel_net: Ipv4Network) -> Void {
        ip::ensure_v4_addr_on_link(ipa, flannel_net, self.link.as_index())?;
        netlink_ng::link_set_up(self.link.as_index())?;
        // todo ensure mac address
        Ok(())
    }
    pub fn add_arp(&self, n: Neighbor) -> Void {
        println!("add arp: {:?}", n);
        netlink_ng::neigh_set(&Neigh {
            link_index: self.link.as_index(),
            state: netlink_ng::NUD_PERMANENT,
            type_: netlink_ng::RTN_UNICAST as u32,
            ip: IpAddr::V4(n.ip),
            hardware_addr: n.mac,
            ..Default::default()
        })?;
        Ok(())
    }
    pub fn add_fdb(&self, n: Neighbor) -> Void {
        println!("add fdb: {:?}", n);
        netlink_ng::neigh_set(&Neigh {
            link_index: self.link.as_index(),
            family: netlink_ng::AF_BRIDGE as u8,
            state: netlink_ng::NUD_PERMANENT,
            flags: netlink_ng::NTF_SELF as u32,
            ip: IpAddr::V4(n.ip),
            hardware_addr: n.mac,
            ..Default::default()
        })?;
        Ok(())
    }
}

fn ensure_vxlan_link(link: &Link) -> Res<Link> {
    if let Err(err) = netlink_ng::link_add(link) {
        if let Some(err) = err.downcast_ref::<std::io::Error>() {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let existing = netlink_ng::link_by_name(link.link_attrs.name.as_str()).context(link.link_attrs.name.clone())?.ok_or(anyhow!("link not found"))?;
                if vxlan_links_incompat(&link, &existing) {
                    return Ok(existing);
                }

                netlink_ng::link_del(existing.as_index()).context("link del failed")?;
                netlink_ng::link_add(&link).context("link add failed")?;
            }
        } else {
            Err(err)?;
        }
    }
    netlink_ng::link_by_name(link.link_attrs.name.as_str())?.ok_or(anyhow!("link not found"))
}

fn vxlan_links_incompat(l1: &Link, l2: &Link) -> bool {
    if l1.kind() != l2.kind() {
        return false;
    }
    let v1 = unwrap_enum!(&l1.link_kind, LinkKind::Vxlan);
    let v2 = unwrap_enum!(&l2.link_kind, LinkKind::Vxlan);
    if v1.is_none() || v2.is_none() { return false; }
    let v1 = v1.unwrap();
    let v2 = v2.unwrap();
    if v1.vxlan_id != v2.vxlan_id {
        return false;
    }
    if v1.vtep_dev_index > 0 && v2.vtep_dev_index > 0 && v1.vtep_dev_index != v2.vtep_dev_index {
        return false;
    }
    if v1.src_addr.is_some() && v2.src_addr.is_some() && v1.src_addr.unwrap() != v2.src_addr.unwrap() {
        return false;
    }
    if v1.group.is_some() && v2.group.is_some() && v1.group.unwrap() != v2.group.unwrap() {
        return false;
    }
    if v1.l2miss != v2.l2miss {
        return false;
    }
    if v1.port > 0 && v2.port > 0 && v1.port != v2.port {
        return false;
    }
    if v1.gbp != v2.gbp {
        return false;
    }

    return true;
}


#[cfg(test)]
mod tests {
    use macaddr::MacAddr6;
    use netlink_ng::{addr_list, link_by_name};
    use netlink_ng::nl_type::FAMILY_V4;

    use crate::backend::vxlan::device::{Neighbor, VxlanDevice, VxlanDeviceAttrs};

    #[test]
    fn test_add_fdb() {
        println!(">>>>>>>>>>>>>>>>....");
        let eth0_link = link_by_name("eth0").unwrap().unwrap();
        let addr = addr_list(eth0_link.as_index(), FAMILY_V4).unwrap();
        let addr = addr.first().unwrap();
        println!("eth0: {:?}", eth0_link);
        println!("addr: {:?}", addr);
        let dev = VxlanDevice::new(VxlanDeviceAttrs {
            vni: 1,
            name: "vxlan1".to_string(),
            mtu: 1500,
            vtep_index: eth0_link.as_index(),
            vtep_addr: addr.ipnet.ip(),
            vtep_port: 0,
            gbp: false,
        }, false).unwrap();
        println!("dev: {:?}", dev);

        dev.configure("172.17.4.0/24".parse().unwrap(), "172.17.0.0/16".parse().unwrap()).unwrap();
        let mac = "26:7a:56:57:d5:60".parse::<MacAddr6>().unwrap();
        println!("mac: {:?}", mac);

        dev.add_fdb(Neighbor {
            ip: "198.19.249.136".parse().unwrap(),
            // mac: "ce:61:2e:f7:65:7d".parse().unwrap(),
            mac: "26:7a:56:57:d5:60".parse().unwrap(),
        }).unwrap();
    }
}