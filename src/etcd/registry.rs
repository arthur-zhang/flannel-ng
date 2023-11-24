use std::fmt::{Debug, Formatter};
use std::ops::Add;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::bail;
use etcd_client::{
    Client, Compare, CompareOp, EventType, GetOptions, KeyValue, KvClient, PutOptions, Txn, TxnOp,
    WatchOptions,
};
use ipnetwork::{Ipv4Network, Ipv6Network};
use regex::Regex;

use crate::lease::{Lease, LeaseAttrs};

#[macro_export]
macro_rules! join_path {
    ($($segment:expr),+) => {{
        let mut path = std::path::PathBuf::new();
        $(
            path.push($segment);
        )+
        path.to_string_lossy().into_owned()
    }};
}

#[derive(Debug, Clone, Default)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    pub keyfile: String,
    pub certfile: String,
    pub ca_file: String,
    pub prefix: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone)]
pub struct EtcdSubnetRegistry {
    etcd_cfg: EtcdConfig,
    client: Client,
    kv_client: KvClient,
}

impl Debug for EtcdSubnetRegistry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EtcdSubnetRegistry")
            .field("etcd_cfg", &self.etcd_cfg)
            .finish()
    }
}

impl EtcdSubnetRegistry {
    pub async fn new(etcd_cfg: EtcdConfig) -> anyhow::Result<Self> {
        let (client, kv_client) = new_etcd_client(&etcd_cfg).await?;
        Ok(Self {
            etcd_cfg,
            client,
            kv_client,
        })
    }
    pub async fn get_network_config(&mut self) -> anyhow::Result<String> {
        let key = Path::new(&self.etcd_cfg.prefix).join("config");
        let key = key
            .to_str()
            .ok_or(anyhow::anyhow!("key is not a valid UTF-8 string"))?;
        let resp = self.kv_client.get(key, None).await?;
        if let Some(kv) = resp.kvs().first() {
            Ok(kv.value_str()?.to_string())
        } else {
            Ok("".to_string())
        }
    }

    pub async fn get_subnet(&mut self, sn: Ipv4Network) -> anyhow::Result<Option<(Lease, i64)>> {
        let key = join_path!(
            &self.etcd_cfg.prefix,
            "subnets",
            format!("{}-{}", sn.ip(), sn.prefix())
        );
        let resp = self.kv_client.get(key, None).await?;
        if resp.kvs().is_empty() {
            return Ok(None);
        }
        let kv = resp.kvs().first().unwrap();

        let ttl_resp = self.client.lease_time_to_live(kv.lease(), None).await?;
        let lease = kv_to_ip_lease(kv, ttl_resp.ttl())?;
        let revision = resp.header().map(|it| it.revision()).unwrap_or_default();
        Ok(Some((lease, revision)))
    }
    pub async fn get_subnets(&mut self) -> anyhow::Result<(Vec<Lease>, i64)> {
        let key = Path::new(&self.etcd_cfg.prefix).join("subnets");
        let key = key
            .to_str()
            .ok_or(anyhow::anyhow!("key is not a valid UTF-8 string"))?;
        let get_opts = GetOptions::new().with_prefix();
        let mut leases = vec![];
        let resp = self.kv_client.get(key, Some(get_opts)).await?;
        if resp.count() == 0 {
            return Ok((vec![], 0));
        }

        for kv in resp.kvs().iter() {
            let ttl_resp = self.client.lease_time_to_live(kv.lease(), None).await;
            if let Ok(ttl_resp) = ttl_resp {
                let lease = kv_to_ip_lease(kv, ttl_resp.ttl()).unwrap();
                leases.push(lease);
            }
        }
        let revision = resp.header().map(|it| it.revision()).unwrap_or_default();
        Ok((leases, revision))
    }
    pub async fn create_subnet(
        &mut self,
        sn: &Ipv4Network,
        attrs: &LeaseAttrs,
        subnet_ttl: Duration,
    ) -> anyhow::Result<SystemTime> {
        let key = join_path!(
            &self.etcd_cfg.prefix,
            "subnets",
            format!("{}-{}", sn.ip(), sn.prefix())
        );
        let value = serde_json::to_vec(attrs)?;
        println!("create subnet: key: {}", key);
        println!("create subnet: value: {}", String::from_utf8_lossy(&value));

        let lease_resp = self
            .client
            .lease_grant(subnet_ttl.as_secs() as i64, None)
            .await?;

        let txn = Txn::new();
        let cond = Compare::version(key.clone(), CompareOp::Equal, 0);
        let req = TxnOp::put(
            key.into_bytes(),
            value,
            Some(PutOptions::new().with_lease(lease_resp.id())),
        );
        let txn = txn.when(vec![cond]).and_then(vec![req]);
        match self.client.txn(txn).await {
            Err(err) => {
                println!("txn_resp is err: {:?}", err);
                let _ = self.client.lease_revoke(lease_resp.id()).await;
                bail!(err)
            }
            Ok(txn_resp) => {
                println!("txn_resp: {:?}", txn_resp);
                if !txn_resp.succeeded() {
                    let _ = self.client.lease_revoke(lease_resp.id()).await;
                    // todo, process duplicate error code
                    bail!("txn_resp is not succeeded")
                }

                let exp = SystemTime::now().add(Duration::from_secs(lease_resp.ttl() as u64));
                Ok(exp)
            }
        }
    }
    pub async fn leases_watch_reset(&mut self) -> anyhow::Result<LeaseWatchResult> {
        let (leases, index) = self.get_subnets().await?;

        Ok(LeaseWatchResult {
            snapshot: leases,
            events: vec![],
            cursor: index,
        })
    }

    pub async fn watch_subnet(
        &mut self,
        sn: Ipv4Network,
        since: i64,
        tx: tokio::sync::mpsc::Sender<Vec<LeaseWatchEvent>>,
    ) -> anyhow::Result<()> {
        let key = join_path!(
            &self.etcd_cfg.prefix,
            "subnets",
            format!("{}-{}", sn.ip(), sn.prefix())
        );
        let (_, mut watch_stream) = self
            .client
            .watch(
                key,
                Some(WatchOptions::new().with_prefix().with_start_revision(since)),
            )
            .await?;
        while let Some(watch_resp) = watch_stream.message().await? {
            if watch_resp.canceled() {
                continue;
            }
            let mut events = vec![];
            for event in watch_resp.events() {
                let subnet_event = self.parse_subnet_watch_resp(&event).await?;
                if let Some(subnet_event) = subnet_event {
                    events.push(subnet_event);
                }
            }
            if !events.is_empty() {
                tx.send(events).await?;
            }
        }
        Ok(())
    }
    pub async fn watch_subnets(
        &mut self,
        since: i64,
        tx: tokio::sync::mpsc::Sender<Vec<LeaseWatchResult>>,
    ) -> anyhow::Result<()> {
        let key = join_path!(&self.etcd_cfg.prefix, "subnets");
        let (_, mut watcher_stream) = self
            .client
            .watch(
                key,
                Some(WatchOptions::new().with_prefix().with_start_revision(since)),
            )
            .await?;
        while let Some(resp) = watcher_stream.message().await? {
            if resp.canceled() {
                break;
            }
            let mut events = vec![];
            for event in resp.events() {
                let subnet_event = self.parse_subnet_watch_resp(&event).await?;
                if let Some(subnet_event) = subnet_event {
                    events.push(LeaseWatchResult {
                        events: vec![subnet_event],
                        cursor: resp.header().map(|it| it.revision()).unwrap_or_default(),
                        ..Default::default()
                    })
                }
            }
            if !events.is_empty() {
                tx.send(events).await?;
            }
        }
        Ok(())
    }

    async fn parse_subnet_watch_resp(
        &mut self,
        ev: &etcd_client::Event,
    ) -> anyhow::Result<Option<LeaseWatchEvent>> {
        println!("ev: {:?}", ev);
        let kv = ev.kv().ok_or(anyhow::anyhow!("invalid kv"))?;
        println!("kv: {}", kv.key_str()?);
        let (sn4, _) = extract_ip_and_subnet(kv.key_str()?)?;
        match ev.event_type() {
            EventType::Delete => Ok(Some(LeaseWatchEvent {
                lease: Lease {
                    enable_ipv4: true,
                    subnet: sn4,
                    ..Default::default()
                },
                event_type: EventType::Delete,
            })),
            EventType::Put => {
                let attrs: LeaseAttrs = serde_json::from_slice(kv.value())?;
                let lease_resp = self.client.lease_time_to_live(kv.lease(), None).await?;
                if lease_resp.ttl() < 0 {
                    return Ok(None);
                }
                let exp = SystemTime::now().add(Duration::from_secs(lease_resp.ttl() as u64));
                Ok(Some(LeaseWatchEvent {
                    lease: Lease {
                        enable_ipv4: true,
                        subnet: sn4,
                        expiration: exp,
                        attrs,
                        ..Default::default()
                    },
                    event_type: EventType::Put,
                }))
            }
        }
    }

    pub async fn update_subnet(
        &mut self,
        subnet: Ipv4Network,
        attrs: &LeaseAttrs,
        ttl: Duration,
    ) -> anyhow::Result<SystemTime> {
        let key = join_path!(
            &self.etcd_cfg.prefix,
            "subnets",
            format!("{}-{}", subnet.ip(), subnet.prefix())
        );
        let value = serde_json::to_vec(attrs)?;
        println!("update subnet: key: {}", key);
        println!("update subnet: value: {}", String::from_utf8_lossy(&value));

        let lease_resp = self.client.lease_grant(ttl.as_secs() as i64, None).await?;

        if let Err(err) = self
            .kv_client
            .put(
                key,
                value,
                Some(PutOptions::new().with_lease(lease_resp.id())),
            )
            .await
        {
            let _ = self.client.lease_revoke(lease_resp.id()).await;
            bail!(err)
        }
        let exp = SystemTime::now().add(Duration::from_secs(lease_resp.ttl() as u64));
        Ok(exp)
    }
}

#[derive(Debug, Default)]
pub struct LeaseWatchResult {
    pub snapshot: Vec<Lease>,
    pub events: Vec<LeaseWatchEvent>,
    pub cursor: i64,
}

#[derive(Debug)]
pub struct LeaseWatchEvent {
    pub lease: Lease,
    pub event_type: EventType,
}

fn kv_to_ip_lease(kv: &KeyValue, ttl: i64) -> anyhow::Result<Lease> {
    let (sn4, sn6) = extract_ip_and_subnet(kv.key_str()?)?;
    let lease_attrs: LeaseAttrs = serde_json::from_slice(kv.value())?;
    let exp = SystemTime::now().add(Duration::from_secs(ttl as u64));
    Ok(Lease {
        enable_ipv4: true,
        enable_ipv6: sn6.is_some(),
        subnet: sn4,
        ipv6_subnet: sn6,
        expiration: exp,
        attrs: lease_attrs,
    })
}

fn extract_ip_and_subnet(input: &str) -> anyhow::Result<(Ipv4Network, Option<Ipv6Network>)> {
    let re = Regex::new(r"(\d+\.\d+.\d+.\d+)-(\d+)(?:&([a-f\d:]+)-(\d+))?$").unwrap();
    let caps = re
        .captures(input)
        .ok_or(anyhow::anyhow!("invalid subnet"))?;

    if caps.len() != 5 {
        bail!("invalid subnet")
    }
    let ip_address = caps.get(1).unwrap().as_str();
    let subnet_length = caps.get(2).unwrap().as_str();
    let sn4: Ipv4Network = format!("{}/{}", ip_address, subnet_length).parse()?;
    if caps.get(3).is_none() || caps.get(4).is_none() {
        return Ok((sn4, None));
    }
    let ip_address_v6 = caps.get(3).unwrap().as_str();
    let subnet_length_v6 = caps.get(4).unwrap().as_str();

    let mut sn6: Option<Ipv6Network> = None;
    if !ip_address_v6.is_empty() && !subnet_length_v6.is_empty() {
        sn6 = Some(format!("{}/{}", ip_address_v6, subnet_length_v6).parse()?);
    }
    Ok((sn4, sn6))
}

pub async fn new_etcd_client(etcd_cfg: &EtcdConfig) -> anyhow::Result<(Client, KvClient)> {
    let client = Client::connect(&etcd_cfg.endpoints, None).await?;
    let kv_client = client.kv_client();
    Ok((client, kv_client))
}

#[cfg(test)]
mod tests {
    use crate::etcd::registry::extract_ip_and_subnet;
    use crate::etcd::{EtcdConfig, EtcdSubnetRegistry};

    #[tokio::test]
    async fn test_get_network_config() {
        let etcd_cfg = EtcdConfig {
            endpoints: vec!["198.19.249.101:2379".to_string()],
            prefix: "/coreos.com/network/".to_string(),
            ..Default::default()
        };
        let mut etcd_subnet_registry = EtcdSubnetRegistry::new(etcd_cfg).await.unwrap();
        let config = etcd_subnet_registry.get_network_config().await.unwrap();
        println!("{}", config);
        let (leases, revision) = etcd_subnet_registry.get_subnets().await.unwrap();
        println!("{:?}", leases);
        println!("{:?}", revision);
    }

    #[test]
    fn test_extract_ip_and_subnet() {
        let (ip, ipv6) =
            extract_ip_and_subnet("coreos.com/network/subnets/172.17.43.0-24").unwrap();
        println!("ip: {:?}, ipv6: {:?}", ip, ipv6);
        let (ip, ipv6) = extract_ip_and_subnet(
            "coreos.com/network/subnets/172.17.43.0-24&abcd:ef01:2345:6789:abcd:ef01:2345:6789-64",
        )
        .unwrap();
        println!("ip: {:?}, ipv6: {:?}", ip, ipv6);
    }

    #[tokio::test]
    async fn test_leases_watch_reset() -> anyhow::Result<()> {
        let etcd_cfg = EtcdConfig {
            endpoints: vec!["198.19.249.101:2379".to_string()],
            prefix: "/coreos.com/network/".to_string(),
            ..Default::default()
        };
        let mut etcd_subnet_registry = EtcdSubnetRegistry::new(etcd_cfg).await.unwrap();
        let result = etcd_subnet_registry.leases_watch_reset().await?;
        println!("{:#?}", result);

        Ok(())
    }

    #[tokio::test]
    async fn test_watch_subnets() -> anyhow::Result<()> {
        let etcd_cfg = EtcdConfig {
            endpoints: vec!["198.19.249.101:2379".to_string()],
            prefix: "/coreos.com/network/".to_string(),
            ..Default::default()
        };
        let mut etcd_subnet_registry = EtcdSubnetRegistry::new(etcd_cfg).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        tokio::spawn(async move {
            loop {
                let result = rx.recv().await;
                println!("receive lease change {:?}", result);
            }
        });
        let result = etcd_subnet_registry.watch_subnets(0, tx).await?;

        println!("{:#?}", result);

        Ok(())
    }
}
