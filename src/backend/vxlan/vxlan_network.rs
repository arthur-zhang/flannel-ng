use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use etcd_client::EventType;
use ipnetwork::IpNetwork;
use macaddr::MacAddr6;
use netlink_ng::Route;
use serde::{Deserialize, Serialize};

use crate::backend::{ExternalInterface, Network};
use crate::backend::vxlan::device::{Neighbor, VxlanDevice};
use crate::lease::Lease;
use crate::subnet::{SubnetManager, TSubnetManager};

pub struct VxlanNetwork {
    pub subnet_lease: Lease,
    pub ext_iface: ExternalInterface,
    pub device: VxlanDevice,
    pub subnet_manager: TSubnetManager,
    pub mtu: u32,
}

impl VxlanNetwork {
    pub fn new(subnet_manager: SubnetManager, external_interface: ExternalInterface, vxlan_device: VxlanDevice,
               lease: Lease, mtu: u32,
    ) -> Self {
        Self {
            subnet_manager: Arc::new(subnet_manager),
            ext_iface: external_interface,
            device: vxlan_device,
            subnet_lease: lease,
            mtu,
        }
    }
}

#[async_trait]
impl Network for VxlanNetwork {
    fn lease(&self) -> &Lease {
        &self.subnet_lease
    }

    fn mtu(&self) -> u32 {
        self.ext_iface.iface.mtu
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn({
            let subnet_manager = self.subnet_manager.clone();
            let own_lease = self.subnet_lease.clone();
            async move {
                let _ = subnet_manager.watch_leases(&own_lease, tx).await;
            }
        });

        while let Some(events) = rx.recv().await {
            for e in events {
                println!(">>>>>>>>>>>>>event: {:?}", e);
                // 172.17.43.0/24
                let sn = e.lease.subnet;
                let attrs = e.lease.attrs;
                if attrs.backend_type.is_none() || attrs.backend_type.as_ref().unwrap() != "vxlan" {
                    continue;
                }

                if attrs.backend_data.is_none() {
                    continue;
                }
                let backend_data_str = serde_json::to_vec(&attrs.backend_data)?;
                let vxlan_attrs = serde_json::from_slice::<VxlanLeaseAttrs>(&backend_data_str)?;
                println!("vxlan_attrs: {:?}", vxlan_attrs);

                let vtep_mac = MacAddr6::from_str(&vxlan_attrs.vtep_mac)?;

                match e.event_type {
                    EventType::Put => {
                        println!("processing put event.............{:?}", attrs);
                        self.device.add_arp(Neighbor { ip: sn.ip(), mac: vtep_mac })?;
                        self.device.add_fdb(Neighbor { ip: attrs.public_ip, mac: vtep_mac })?;

                        let mut route = Route {
                            link_index: self.device.link.as_index(),
                            scope: netlink_ng::RT_SCOPE_UNIVERSE,
                            dst: Some(IpNetwork::V4(sn)),
                            gw: Some(IpAddr::V4(sn.ip())),
                            ..Default::default()
                        };
                        route.flags |= 0x4;// RTNH_F_ONLINK

                        netlink_ng::route_replace(&route)?;
                    }
                    EventType::Delete => {
                        println!("delete event.............")
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VxlanLeaseAttrs {
    #[serde(rename = "VNI")]
    pub vni: u32,

    #[serde(rename = "VtepMAC")]
    pub vtep_mac: String,
}

