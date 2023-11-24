use std::net::Ipv4Addr;
use std::time::SystemTime;

use ipnetwork::{Ipv4Network, Ipv6Network};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Lease {
    pub enable_ipv4: bool,
    pub subnet: Ipv4Network,
    pub enable_ipv6: bool,
    pub ipv6_subnet: Option<Ipv6Network>,
    pub attrs: LeaseAttrs,
    pub expiration: SystemTime,
    // pub asof: i64,
}

impl Default for Lease {
    fn default() -> Self {
        Self {
            enable_ipv4: false,
            enable_ipv6: false,
            subnet: "0.0.0.0/0".parse().unwrap(),
            ipv6_subnet: None,
            attrs: LeaseAttrs {
                public_ip: "0.0.0.0".parse().unwrap(),
                public_ipv6: None,
                backend_type: None,
            },
            expiration: SystemTime::now(),
            // asof: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LeaseAttrs {
    #[serde(rename = "PublicIP")]
    pub public_ip: Ipv4Addr,
    #[serde(rename = "PublicIPv6", skip_serializing_if = "Option::is_none")]
    pub public_ipv6: Option<Ipv6Network>,
    #[serde(rename = "BackendType", skip_serializing_if = "Option::is_none")]
    pub backend_type: Option<String>,
}

impl Default for LeaseAttrs {
    fn default() -> Self {
        Self {
            public_ip: Ipv4Addr::UNSPECIFIED,
            public_ipv6: None,
            backend_type: None,
        }
    }
}
