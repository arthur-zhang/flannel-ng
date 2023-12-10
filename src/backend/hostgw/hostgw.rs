use std::sync::Arc;
use async_trait::async_trait;

use crate::backend::{Backend, ExternalInterface, Network};
use crate::backend::hostgw::router_network::RouterNetwork;
use crate::lease::LeaseAttrs;
use crate::prelude::*;
use crate::subnet::config::Config;
use crate::subnet::SubnetManager;

pub struct HostGw {
    pub subnet_manager: SubnetManager,
    pub ext_iface: ExternalInterface,
}

impl HostGw {
    pub fn new(subnet_manager: SubnetManager, ext_iface: ExternalInterface) -> Res<Self> {
        if ext_iface.ext_addr != Some(ext_iface.iface_addr) {
            return Err(anyhow::anyhow!("your PublicIP differs from interface IP, meaning that probably you're on a NAT, which is not supported by host-gw backend"));
        }
        Ok(Self { subnet_manager, ext_iface })
    }
}

#[async_trait]
impl Backend for HostGw {
    async fn register_network(&mut self, _config: &Config) -> anyhow::Result<Box<dyn Network + Send>> {
        let attrs = LeaseAttrs {
            backend_type: Some("host-gw".to_string()),
            public_ip: self.ext_iface.ext_addr.unwrap(),
            ..Default::default()
        };

        let l = self.subnet_manager.acquire_lease(&attrs).await?;


        let network = RouterNetwork {
            subnet_lease: l,
            ext_iface: self.ext_iface.clone(),
            routes: vec![],
            subnet_manger: Arc::new(self.subnet_manager.clone()),
            mtu: self.ext_iface.iface.mtu,
            link_index: self.ext_iface.iface.index,
            backend_type: "host-gw",
        };

        Ok(Box::new(network))
    }
}

