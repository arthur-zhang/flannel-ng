use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ipnetwork::Ipv4Network;
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, ExternalInterface, Network};
use crate::backend::vxlan::device::{VxlanDevice, VxlanDeviceAttrs};
use crate::backend::vxlan::vxlan_network::{VxlanLeaseAttrs, VxlanNetwork};
use crate::lease::LeaseAttrs;
use crate::subnet::config::Config;
use crate::subnet::SubnetManager;

#[derive(Debug)]
pub struct Vxlan {
    pub subnet_manager: SubnetManager,
    pub ext_iface: ExternalInterface,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VxlanConfig {
    #[serde(rename = "Type", default)]
    pub vxlan_type: String,

    #[serde(rename = "VNI", default)]
    pub vni: u32, // Vxlan Network Identifier

    #[serde(rename = "Port", default)]
    pub port: u16, // UDP port

    #[serde(rename = "GBP", default)]
    pub gbp: bool, // Group Based Policy

    #[serde(rename = "DirectRouting", default)]
    pub direct_routing: bool, // Direct Routing

    #[serde(rename = "MTU", default)]
    pub mtu: Option<u32>, // Maximum Transmission Unit

    // #[serde(rename = "MacPrefix", default="0E-2A")]
    // pub mac_prefix: Option<String>, // MAC Prefix, only used on Windows
}

impl Vxlan {
    pub fn new(subnet_manager: SubnetManager, ext_iface: ExternalInterface) -> Self {
        Vxlan {
            subnet_manager,
            ext_iface,
        }
    }
}

#[async_trait]
impl Backend for Vxlan {
    async fn register_network(&mut self, config: &Config) -> anyhow::Result<Box<dyn Network + Send>> {
        let backend = (config.backend.as_ref()).ok_or(anyhow::anyhow!("backend is not defined"))?;
        let backend_json = serde_json::to_string(&backend)?;
        let vxlan_config = {
            let mut vxlan_config: VxlanConfig = serde_json::from_str(&backend_json)?;
            if vxlan_config.vni == 0 {
                vxlan_config.vni = 1;
            }
            if vxlan_config.mtu.is_none() {
                vxlan_config.mtu = Some(self.ext_iface.iface.mtu);
            }
            if vxlan_config.port == 0 {
                vxlan_config.port = 8472;
            }
            vxlan_config
        };

        println!("vxlan_config: {:?}", vxlan_config);

        let dev_attrs = VxlanDeviceAttrs {
            vni: vxlan_config.vni,
            name: format!("flannel.{}", vxlan_config.vni),
            mtu: vxlan_config.mtu.unwrap(),
            vtep_index: self.ext_iface.iface.index,
            vtep_addr: IpAddr::V4(self.ext_iface.iface_addr),
            vtep_port: vxlan_config.port,
            gbp: vxlan_config.gbp,
        };
        let vxlan_device = VxlanDevice::new(dev_attrs, vxlan_config.direct_routing)?;


        let lease_attr = {
            let backend_data = serde_json::to_value(&VxlanLeaseAttrs {
                vni: vxlan_config.vni,
                vtep_mac: format!("{}", vxlan_device.link.link_attrs.hardware_addr.unwrap()),
            })?;

            let attr = LeaseAttrs {
                public_ip: self.ext_iface.iface_addr,
                public_ipv6: None,
                backend_type: Some("vxlan".to_string()),
                backend_data: Some(backend_data),
            };
            attr
        };
        // 172.17.43.1/24
        let lease = self.subnet_manager.acquire_lease(&lease_attr).await?;
        // 172.17.0.0/16
        let net = config.get_flannel_network()?;

        // 172.17.43.1/32
        vxlan_device.configure(Ipv4Network::new(lease.subnet.ip(), 32)?, net)?;


        Ok(Box::new(VxlanNetwork {
            subnet_lease: lease,
            ext_iface: self.ext_iface.clone(),
            device: vxlan_device,
            subnet_manager: Arc::new(self.subnet_manager.clone()),
            mtu: vxlan_config.mtu.unwrap(),
        }))
    }
}
