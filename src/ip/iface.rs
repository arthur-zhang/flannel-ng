use anyhow::{anyhow, bail};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_default_gateway_interface() {
        let link = get_default_gateway_interface().unwrap();
        println!("{:?}", link)
    }
}
