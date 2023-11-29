use std::net::IpAddr;

use anyhow::{anyhow, bail, Context};
use ipnetwork::{IpNetwork, Ipv4Network};
use netlink_ng::{Addr, LinkIndex};
use netlink_ng::nl_type::FAMILY_V4;

use crate::backend::Interface;

pub fn get_default_gateway_interface() -> anyhow::Result<Option<Interface>> {
    let routes = netlink_ng::route_list(None, FAMILY_V4)?;
    for route in routes {
        if route.dst.is_none() || route.dst.unwrap() == "0.0.0.0/0".parse().unwrap() {
            if route.link_index == 0 {
                bail!("Found default route but could not determine interface")
            }

            let link = netlink_ng::link_by_index(route.link_index)?;
            let link = link.ok_or(anyhow!("link not found"))?;

            return Ok(Some(Interface {
                index: link.link_attrs.index,
                mtu: link.link_attrs.mtu,
                name: link.link_attrs.name,
            }));
        }
    }
    Ok(None)
}

pub fn ensure_v4_addr_on_link(ip: Ipv4Network, ipn: Ipv4Network, link_id: LinkIndex) -> anyhow::Result<()> {
    let addr = Addr {
        ipnet: IpNetwork::V4(ip),
        ..Default::default()
    };
    let existing_addrs = netlink_ng::addr_list(link_id, FAMILY_V4).context("addr list failed")?;

    let mut has_addr = false;
    for existing_addr in &existing_addrs {
        println!("existing_addr: {:?}", existing_addr);
        if existing_addr == &addr {
            has_addr = true;
            continue;
        }
        let existing_addr_ipv4 = match existing_addr.ipnet.ip() {
            IpAddr::V4(ip) => { ip }
            IpAddr::V6(_) => {
                continue;
            }
        };
        if ipn.contains(existing_addr_ipv4) {
            netlink_ng::addr_del(link_id, existing_addr).context("addr del failed")?;
        }
    }
    println!("has_addr: {:?}", has_addr);
    if !has_addr {
        netlink_ng::addr_add(link_id, &addr).context("addr add failed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use netlink_ng::{link_list, TryAsLinkIndex};

    use super::*;

    #[test]
    fn test_get_default_gateway_interface() {
        let link = get_default_gateway_interface().unwrap();
        println!("{:?}", link)
    }

    #[test]
    fn test_ensure_v4_addr_on_link() {
        let list = link_list().unwrap();
        for link in list {
            println!("link.......{:?}", link.attrs().name)
        }
        let link_index = "vxlan0".try_as_index().unwrap().unwrap();
        let ip = "10.0.0.5/24".parse().unwrap();
        let ipn = "10.0.0.0/16".parse().unwrap();
        let res = ensure_v4_addr_on_link(ip, ipn, link_index);
        println!("res: {:?}", res)
    }
}
