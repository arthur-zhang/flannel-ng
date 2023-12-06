use std::net::IpAddr;

use async_trait::async_trait;
use etcd_client::EventType;
use ipnetwork::IpNetwork;
use netlink_ng::Route;
use nix::libc;

use crate::backend::{ExternalInterface, Network};
use crate::lease::Lease;
use crate::subnet::TSubnetManager;

pub struct RouterNetwork {
    pub subnet_lease: Lease,
    pub ext_iface: ExternalInterface,
    pub routes: Vec<Route>,
    pub subnet_manger: TSubnetManager,
    pub mtu: u32,
    pub link_index: u32,
    pub backend_type: &'static str,
}

#[async_trait]
impl Network for RouterNetwork {
    fn lease(&self) -> &Lease {
        &self.subnet_lease
    }

    fn mtu(&self) -> u32 {
        self.mtu
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn({
            let subnet_manager = self.subnet_manger.clone();
            let own_lease = self.subnet_lease.clone();
            async move {
                let _ = subnet_manager.watch_leases(&own_lease, tx).await;
            }
        });
        while let Some(events) = rx.recv().await {
            for event in events {
                match event.event_type {
                    EventType::Put => {
                        if event.lease.attrs.backend_type.as_ref().map(|it| it.as_str()) != Some(self.backend_type) {
                            println!("ignore backend type: {:?}", event.lease.attrs.backend_type);
                            continue;
                        }
                        let route = self.get_route(&event.lease);
                        self.route_add(netlink_ng::nl_type::FAMILY_V4, &route)?;
                    }
                    EventType::Delete => {
                        println!("todo!!!!!!!!!!! delete event: {:?}", event);
                        if event.lease.attrs.backend_type.as_ref().map(|it| it.as_str()) != Some(self.backend_type) {
                            println!("ignore backend type: {:?}", event.lease.attrs.backend_type);
                            continue;
                        }
                        let route = self.get_route(&event.lease);
                        self.routes.retain(|it| !route_equal(it, &route));

                        if let Err(err) = netlink_ng::route_del(&route) {
                            println!("Error deleting route to {:?}: {:?}", event.lease.subnet, err);
                        }
                    }
                }
            }
        }


        Ok(())
    }
}

impl RouterNetwork {
    fn get_route(&self, lease: &Lease) -> Route {
        return Route {
            dst: Some(IpNetwork::V4(lease.subnet)),
            gw: Some(IpAddr::V4(lease.attrs.public_ip)),
            link_index: self.link_index,
            flags: netlink_ng::RTNH_F_ONLINK as u32,
            ..Default::default()
        };
    }
    fn route_add(&mut self, family: netlink_ng::Family, route: &Route) -> anyhow::Result<()> {
        self.routes.push(route.clone());

        println!("route add: {:?}", route);
        // check existing dst route
        let route_list = netlink_ng::route_list_filtered(family, Some(Route {
            dst: route.dst.clone(),
            ..Default::default()
        }), netlink_ng::RT_FILTER_DST)?;
        println!("route_list: {:?}", route_list);
        // same dst, different gw or link index, delete old route
        if !route_list.is_empty() && !route_equal(&route_list[0], route) {
            netlink_ng::route_del(&route_list[0])?;
            self.routes.retain(|it| !route_equal(it, &route_list[0]));
        }
        let route_list = netlink_ng::route_list_filtered(family, Some(Route {
            dst: route.dst.clone(),
            ..Default::default()
        }), netlink_ng::RT_FILTER_DST)?;

        if !route_list.is_empty() && route_equal(&route_list[0], route) {
            println!("route already exists, skipping.");
        } else {
            netlink_ng::route_add(route)?;
        }

        Ok(())
    }
}

fn route_equal(x: &Route, y: &Route) -> bool {
    x.dst.is_some() && y.dst.is_some() && x.dst.unwrap() == y.dst.unwrap()
        && x.gw.is_some() && y.gw.is_some() && x.gw.unwrap() == y.gw.unwrap()
        && x.link_index == y.link_index
}