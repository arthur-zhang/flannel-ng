use std::fs::DirBuilder;
use std::net::Ipv4Addr;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail};
use etcd_client::EventType;
use ipnetwork::Ipv4Network;
use rand::Rng;
use tokio::time::sleep;

use crate::etcd;
use crate::etcd::{LeaseWatchEvent, LeaseWatchResult};
use crate::lease::{Lease, LeaseAttrs};
use crate::subnet::config::Config;

const RACE_RETRIES: i32 = 10;
const SUBNET_TTL: Duration = Duration::from_secs(24 * 60 * 60);

// pub type Registry = Arc<Mutex<etcd::EtcdSubnetRegistry>>;
pub type Registry = etcd::EtcdSubnetRegistry;

#[derive(Debug, Clone)]
pub struct SubnetManager {
    pub registry: Registry,
    pub prev_subnet: Option<Ipv4Network>,
    pub subnet_lease_renew_margin: u32,
}

fn parse_backend_type(be: &Option<serde_json::Value>) -> anyhow::Result<String> {
    match be {
        None => Ok("udp".into()),
        Some(be) => Ok(be
            .as_object()
            .map(|it| it.get("Type"))
            .flatten()
            .map(|it| it.as_str())
            .flatten()
            .map(|it| it.to_string())
            .unwrap_or("udp".into())),
    }
}

impl SubnetManager {
    pub fn new(
        registry: etcd::EtcdSubnetRegistry,
        prev_subnet: Option<Ipv4Network>,
        subnet_lease_renew_margin: u32,
    ) -> Self {
        Self {
            registry,
            prev_subnet,
            subnet_lease_renew_margin,
        }
    }
    pub async fn get_network_config(&mut self) -> anyhow::Result<Config> {
        let cfg = self.registry.get_network_config().await?;
        println!("cfg: {}", cfg);
        let mut cfg: Config = serde_json::from_str(&cfg)?;
        cfg.enable_ipv4 = true;
        cfg.backend_type = Some(parse_backend_type(&cfg.backend)?);

        self.check_network_config(&mut cfg)?;
        Ok(cfg)
    }
    fn check_network_config(&self, config: &mut Config) -> anyhow::Result<()> {
        if config.enable_ipv4 {
            if config.network.is_none() {
                bail!("please define a correct Network parameter in the flannel config")
            }
            let cfg_network = config.network.as_ref().unwrap();

            if config.subnet_len > 0 {
                if config.subnet_len > 30 {
                    bail!("SubnetLen must be less than /31")
                }

                if config.subnet_len < cfg_network.prefix() + 2 {
                    bail!("network must be able to accommodate at least four subnets")
                }
            } else {
                if cfg_network.prefix() > 28 {
                    bail!("network is too small. Minimum useful network prefix is /28")
                } else if cfg_network.prefix() <= 22 {
                    // Network is big enough to give each host a /24
                    config.subnet_len = 24;
                } else {
                    // Use +2 to provide four hosts per subnet.
                    config.subnet_len = cfg_network.prefix() + 2;
                }
            }
            let subnet_size = 1u32 << (32 - config.subnet_len);
            match config.subnet_min {
                None => {
                    config.subnet_min = Some(cfg_network.ip().add(subnet_size));
                }

                Some(subnet_min) => {
                    if !cfg_network.contains(subnet_min) {
                        bail!("SubnetMin is not in the network range")
                    }
                }
            }
            match config.subnet_max {
                None => {
                    // 172.17.0.0 -> 172.18.0.0
                    // let next_network =  cfg_network.next();;
                    let next_network_ip = cfg_network.next_network().unwrap();
                    // 172.18.0.0 -> 172.17.255.0
                    config.subnet_max = Some(next_network_ip.ip().sub(subnet_size));
                }
                Some(subnet_max) => {
                    if !cfg_network.contains(subnet_max) {
                        bail!("SubnetMax is not in the network range")
                    }
                }
            }
            let mask = 0xFFFFFFFF_u32 << (32 - config.subnet_len);
            let subnet_min = u32::from(config.subnet_min.unwrap());
            if subnet_min != subnet_min & mask {
                bail!("SubnetMin is not aligned to SubnetLen")
            }
            let subnet_max = u32::from(config.subnet_max.unwrap());
            if subnet_max != subnet_max & mask {
                bail!("SubnetMax is not aligned to SubnetLen")
            }
        }
        if config.enable_ipv6 {
            todo!()
        }
        Ok(())
    }
    pub async fn acquire_lease(&mut self, attrs: &LeaseAttrs) -> anyhow::Result<Lease> {
        let config = self.get_network_config().await?;
        for _ in 0..RACE_RETRIES {
            let l = self
                .try_acquire_lease(&config, attrs, attrs.public_ip)
                .await?;
            return Ok(l);
        }
        bail!("Max retries reached trying to acquire a subnet")
    }
    async fn try_acquire_lease(
        &mut self,
        config: &Config,
        attrs: &LeaseAttrs,
        ext_addr: Ipv4Addr,
    ) -> anyhow::Result<Lease> {
        let (leases, _) = self.registry.get_subnets().await?;
        let lease = leases
            .iter()
            .find(|it| it.attrs.public_ip == ext_addr)
            .cloned();
        if let Some(lease) = lease {
            if lease.subnet.ip() >= config.subnet_min.unwrap()
                && lease.subnet.ip() <= config.subnet_max.unwrap()
                && lease.subnet.prefix() == config.subnet_len
            {
                println!("found lease ip in etcd for ip {},{:?}", ext_addr, lease);
                let _ttl = SUBNET_TTL;
                self.registry
                    .update_subnet(lease.subnet, attrs, SUBNET_TTL)
                    .await?;
                return Ok(lease);
            }
        }

        let sn = self.allocate_subnet(config, &leases).await?;

        let exp = self.registry.create_subnet(&sn, attrs, SUBNET_TTL).await?;
        let lease = Lease {
            enable_ipv4: true,
            subnet: sn,
            enable_ipv6: false,
            ipv6_subnet: None,
            attrs: attrs.clone(),
            expiration: exp,
            ..Default::default()
        };
        Ok(lease)
    }
    async fn allocate_subnet(
        &self,
        config: &Config,
        leases: &[Lease],
    ) -> anyhow::Result<Ipv4Network> {
        println!("config>>>>>>>>>>>>>..: {:?}", config);
        let subnet_max = config.subnet_max.unwrap();
        let mut available_ips = vec![];

        let mut sn = Ipv4Network::new(config.subnet_min.unwrap(), config.subnet_len)?;
        while sn.ip() <= subnet_max && available_ips.len() < 100 {
            let mut overlap = false;
            for l in leases {
                if sn.overlaps(l.subnet) {
                    overlap = true;
                    break;
                }
            }
            if !overlap {
                available_ips.push(sn.ip());
            }
            sn = sn.next_network().unwrap();
        }
        if available_ips.is_empty() {
            bail!("no available subnets")
        }
        let i = rand::thread_rng().gen_range(0..available_ips.len());
        return Ok(Ipv4Network::new(*available_ips.get(i).unwrap(), config.subnet_len).unwrap());
    }
    pub fn write_subnet_file(
        path: &Path,
        config: &Config,
        ip_masq: bool,
        sn: Ipv4Network,
        mtu: u32,
    ) -> anyhow::Result<()> {
        let file_name = path
            .file_name()
            .and_then(|it| it.to_str())
            .ok_or(anyhow!("file name should exists"))?;
        let dir = path.parent().ok_or(anyhow!("parent should not be none"))?;
        DirBuilder::new().recursive(true).mode(0o755).create(dir)?;
        let tmp_file_path = dir.join(format!(".{}", file_name));
        let mut tmp_file = std::fs::File::create(&tmp_file_path)?;
        if config.enable_ipv4 {
            use std::io::Write;
            writeln!(tmp_file, "FLANNEL_NETWORK={}", config.network.unwrap())?;
            let next_ip = sn.ip().add(1);
            writeln!(tmp_file, "FLANNEL_SUBNET={}/{}", next_ip, sn.prefix())?;
            writeln!(tmp_file, "FLANNEL_MTU={}", mtu)?;
            writeln!(tmp_file, "FLANNEL_IPMASQ={}", ip_masq)?;
        }
        std::fs::rename(tmp_file_path, path)?;
        Ok(())
    }

    pub async fn watch_leases(
        &self,
        own_lease: &Lease,
        tx: tokio::sync::mpsc::Sender<Vec<LeaseWatchEvent>>,
    ) -> anyhow::Result<()> {
        let mut lease_watcher = LeaseWatcher {
            own_lease,
            leases: vec![],
        };
        let (lease_watch_tx, mut lease_watch_rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn({
            let registry = self.registry.clone();
            async move {
                let _ = Self::watch_leases_inner(registry, lease_watch_tx).await;
            }
        });

        while let Some(watch_results) = lease_watch_rx.recv().await {
            for wr in watch_results {
                let batch = if !wr.events.is_empty() {
                    lease_watcher.update(&wr.events)
                } else if !wr.snapshot.is_empty() {
                    lease_watcher.reset(&wr.snapshot)
                } else {
                    vec![]
                };

                if !batch.is_empty() {
                    tx.send(batch).await?;
                }
            }
        }
        Ok(())
    }
    async fn watch_leases_inner(
        mut registry: Registry,
        tx: tokio::sync::mpsc::Sender<Vec<LeaseWatchResult>>,
    ) -> anyhow::Result<()> {
        let wr = registry.leases_watch_reset().await?;
        let cursor = wr.cursor;
        tx.send(vec![wr]).await?;
        registry.watch_subnets(cursor, tx).await?;
        Ok(())
    }

    pub async fn renew_lease(&mut self, lease: &mut Lease) -> anyhow::Result<()> {
        println!("renew lease: {:?}", lease);
        let exp = self
            .registry
            .update_subnet(lease.subnet, &lease.attrs, SUBNET_TTL)
            .await?;
        lease.expiration = exp;
        println!("renew lease done: {:?}", lease);
        Ok(())
    }

    pub async fn renew_lease_if_needed(&mut self, lease: &mut Lease) -> anyhow::Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        // spawn task to watch lease
        tokio::spawn({
            let mut registry = self.registry.clone();
            let subnet = lease.subnet;
            async move {
                let _ = watch_lease(&mut registry, subnet, tx).await;
            }
        });

        println!("renew margin: {}", self.subnet_lease_renew_margin);
        let renew_margin = Duration::from_secs(60 * self.subnet_lease_renew_margin as u64);
        let now = SystemTime::now();

        use std::ops::Sub;
        let mut dur = lease.expiration.duration_since(now)? - renew_margin;

        loop {
            tokio::select! {
                _ = sleep(dur) => {
                    if let Err(err) = self.renew_lease(lease).await {
                        println!("renew lease error: {:?}", err);
                        dur = Duration::from_secs(3);
                        continue;
                    }
                    dur = lease.expiration.duration_since(SystemTime::now())?.sub(renew_margin);
                    println!("renew lease success: {:?}, next dur {:?}", lease.expiration, dur.as_secs());
                }

                Some(events) = rx.recv() => {
                    for e in events {
                        match e.event_type {
                            EventType::Put => {
                                *lease = e.lease;
                                dur = lease.expiration.duration_since(SystemTime::now())?.sub(renew_margin);
                                println!("lease change, new lease is :{:?}", lease);
                                println!("new duration is: {}", dur.as_secs() as f64 / 3600.0f64);
                            }
                            EventType::Delete => {
                                bail!("lease deleted")
                            }
                        }
                    }
                }
                else => {
                    break;
                }
            }
        }

        Ok(())
    }
}

pub async fn watch_lease(
    registry: &mut Registry,
    sn: Ipv4Network,
    tx: tokio::sync::mpsc::Sender<Vec<LeaseWatchEvent>>,
) -> anyhow::Result<()> {
    let (l, idx) = registry
        .get_subnet(sn)
        .await?
        .ok_or(anyhow!("subnet {} not found", sn))?;
    tx.send(vec![LeaseWatchEvent {
        lease: l,
        event_type: EventType::Put,
    }])
    .await?;
    registry.watch_subnet(sn, idx, tx).await?;
    Ok(())
}

#[derive(Debug)]
pub struct LeaseWatcher<'a> {
    pub own_lease: &'a Lease,
    pub leases: Vec<Lease>,
}

impl<'a> LeaseWatcher<'a> {
    pub fn reset(&mut self, leases: &[Lease]) -> Vec<LeaseWatchEvent> {
        let mut batch = vec![];
        for nl in leases {
            if nl.subnet == self.own_lease.subnet {
                continue;
            }

            let mut found_idx = None;
            for (i, ol) in self.leases.iter().enumerate() {
                if ol.subnet == nl.subnet {
                    found_idx = Some(i);
                    break;
                }
            }

            match found_idx {
                None => {
                    // new lease
                    batch.push(LeaseWatchEvent {
                        event_type: EventType::Put,
                        lease: nl.clone(),
                    });
                }
                Some(idx) => {
                    // already found
                    self.leases.remove(idx);
                }
            }
        }

        // remaining leases are not found, so they are deleted
        let to_del_leases = self
            .leases
            .iter()
            .map(|it| LeaseWatchEvent {
                lease: it.clone(),
                event_type: EventType::Delete,
            })
            .collect::<Vec<_>>();

        batch.extend(to_del_leases);

        self.leases = leases.to_vec();
        return batch;
    }
    pub fn update(&mut self, events: &[LeaseWatchEvent]) -> Vec<LeaseWatchEvent> {
        let mut batch = Vec::new();
        for e in events {
            if e.lease.subnet == self.own_lease.subnet {
                continue;
            }
            match e.event_type {
                EventType::Put => {
                    batch.push(self.add(&e.lease));
                }
                EventType::Delete => {
                    batch.push(self.remove(&e.lease));
                }
            }
        }
        batch
    }

    pub fn add(&mut self, lease: &Lease) -> LeaseWatchEvent {
        for l in &mut self.leases {
            if l.subnet == lease.subnet {
                *l = lease.clone();
                return LeaseWatchEvent {
                    event_type: EventType::Put,
                    lease: lease.clone(),
                };
            }
        }
        self.leases.push(lease.clone());
        return LeaseWatchEvent {
            event_type: EventType::Put,
            lease: lease.clone(),
        };
    }
    pub fn remove(&mut self, lease: &Lease) -> LeaseWatchEvent {
        let mut to_del = None;
        for (i, l) in self.leases.iter().enumerate() {
            if l.subnet == lease.subnet {
                to_del = Some(i);
                break;
            }
        }
        if let Some(i) = to_del {
            return LeaseWatchEvent {
                lease: self.leases.remove(i),
                event_type: EventType::Delete,
            };
        }
        // not found, may not happen
        return LeaseWatchEvent {
            lease: lease.clone(),
            event_type: EventType::Delete,
        };
    }
}

pub trait Add {
    fn add(self, n: u32) -> Self;
}

pub trait Sub {
    fn sub(self, n: u32) -> Self;
}

impl Add for Ipv4Addr {
    fn add(self, n: u32) -> Self {
        Ipv4Addr::from(u32::from(self) + n)
    }
}

impl Sub for Ipv4Addr {
    fn sub(self, n: u32) -> Self {
        Ipv4Addr::from(u32::from(self) - n)
    }
}

pub trait Ipv4NetworkIterator {
    fn next_network(&self) -> Option<Ipv4Network>;
}

// 172.17.0.0/16->172.18.0.0/16
impl Ipv4NetworkIterator for Ipv4Network {
    fn next_network(&self) -> Option<Ipv4Network> {
        if self.prefix() >= 32 {
            return None;
        }
        let next_ip = u32::from(self.ip()) + (1 << (32 - self.prefix()));
        Ipv4Network::new(Ipv4Addr::from(next_ip), self.prefix()).ok()
    }
}

#[cfg(test)]
mod tests {
    use crate::etcd::{EtcdConfig, EtcdSubnetRegistry};

    use super::*;

    #[tokio::test]
    async fn test_subnet_manager() -> anyhow::Result<()> {
        let mut sm = SubnetManager::new(
            EtcdSubnetRegistry::new(EtcdConfig {
                endpoints: vec!["198.19.249.101:2379".into()],
                prefix: "/coreos.com/network/".into(),
                ..Default::default()
            })
            .await?,
            None,
            0,
        );
        let conf = sm.get_network_config().await.unwrap();
        println!("conf: {:?}", conf);
        Ok(())
    }

    #[test]
    fn test_parse_backend_type() {
        let config = r#"{
	"Network": "10.0.0.0/8",
	"SubnetLen": 20,
	"SubnetMin": "10.10.0.0",
	"SubnetMax": "10.99.0.0",
	"Backend": {
		"Type": "vxlan",
		"Port": 7890
	}
}"#;

        let config: Config = serde_json::from_str(config).unwrap();
        println!("config: {:?}", config);
        let be_type = parse_backend_type(&config.backend).unwrap();
        println!("be_type: {}", be_type);

        // let v = json!({"Type": "vxlan"});
        // let be_type = parse_backend_type(&Some(v)).unwrap();
        // println!("be_type: {}", be_type);
    }

    #[test]
    fn test_next_network() {
        let mut n: Ipv4Network = "172.17.0.0/16".parse().unwrap();
        let next_network = n.next_network();
        println!("next_network: {:?}", next_network);
    }

    #[test]
    fn test_lease() {
        let my_lease = Lease {
            enable_ipv4: true,
            subnet: "172.18.1.0/24".parse().unwrap(),
            ..Default::default()
        };
        let mut lw = LeaseWatcher {
            own_lease: &my_lease,
            leases: vec![Lease {
                enable_ipv4: true,
                subnet: "172.18.5.0/24".parse().unwrap(),
                ..Default::default()
            }],
        };

        let snapshot = vec![
            Lease {
                enable_ipv4: true,
                subnet: "172.18.1.0/24".parse().unwrap(),
                ..Default::default()
            },
            Lease {
                enable_ipv4: true,
                subnet: "172.18.2.0/24".parse().unwrap(),
                ..Default::default()
            },
            Lease {
                enable_ipv4: true,
                subnet: "172.18.3.0/24".parse().unwrap(),
                ..Default::default()
            },
        ];

        let events = lw.reset(&snapshot);
        for e in events {
            println!("{:?} -> {:?}", e.event_type, e.lease.subnet);
        }
        println!(">>>>>>>");
        println!(">>>>>>>{:#?}", lw);
    }
}
