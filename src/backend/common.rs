use std::net::{Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;

use crate::lease::Lease;
use crate::subnet::config::Config;

#[async_trait]
pub trait Backend<T>
where
    T: Network + Sized,
{
    async fn register_network(mut self, config: &Config) -> anyhow::Result<T>;
}

#[async_trait]
pub trait Network {
    fn lease(&self) -> &Lease;
    fn mtu(&self) -> u32;
    async fn run(&mut self) -> anyhow::Result<()>;
}

#[derive(Debug, Clone)]
pub struct Interface {
    pub index: u32,
    // Assuming Index is always positive; using u32
    pub mtu: u32,
    // MTU is typically a positive value
    pub name: String,
    // pub hardware_addr: HardwareAddr,
    // pub flags: Flags,
}

#[derive(Debug, Clone)]
pub struct ExternalInterface {
    pub iface: Interface,
    pub iface_addr: Ipv4Addr,
    pub iface_v6_addr: Option<Ipv6Addr>,
    pub ext_addr: Option<Ipv4Addr>,
    pub ext_v6_addr: Option<Ipv6Addr>,
}
