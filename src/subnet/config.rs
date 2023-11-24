use anyhow::bail;
use std::net::{Ipv4Addr, Ipv6Addr};

use ipnetwork::{Ipv4Network, Ipv6Network};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    #[serde(rename = "EnableIPv4", default)]
    pub enable_ipv4: bool,

    #[serde(rename = "EnableIPv6", default)]
    pub enable_ipv6: bool,

    #[serde(rename = "Network", default)]
    pub network: Option<Ipv4Network>,

    #[serde(rename = "IPv6Networks", default)]
    pub ipv6_network: Option<Ipv6Network>,

    // #[serde(rename = "Networks", default)]
    // pub networks: Option<Vec<Ipv4Network>>,

    // #[serde(rename = "IPv6Networks", default)]
    // pub ipv6_networks: Option<Vec<Ipv6Network>>,
    #[serde(rename = "SubnetMin", default)]
    pub subnet_min: Option<Ipv4Addr>,

    #[serde(rename = "SubnetMax", default)]
    pub subnet_max: Option<Ipv4Addr>,

    #[serde(rename = "IPv6SubnetMin", default)]
    pub ipv6_subnet_min: Option<Ipv6Addr>,

    #[serde(rename = "IPv6SubnetMax", default)]
    pub ipv6_subnet_max: Option<Ipv6Addr>,

    #[serde(rename = "SubnetLen", default)]
    pub subnet_len: u8,

    #[serde(rename = "IPv6SubnetMin", default)]
    pub ipv6_subnet_len: u32,

    // #[serde(rename = "BackendType", skip_serializing_if = "Option::is_none")]
    pub backend_type: Option<String>,

    #[serde(rename = "Backend", skip_serializing_if = "Option::is_none")]
    pub backend: Option<JsonValue>,
}

impl Config {
    pub fn get_flannel_network(&self) -> anyhow::Result<Ipv4Network> {
        if self.network.is_none() {
            bail!("could not find an ipv4 network in the flannel configuration")
        }
        let network = self.network.unwrap();
        Ok(network)
    }
}
