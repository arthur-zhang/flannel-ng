use crate::backend::{Backend, ExternalInterface};
use crate::backend::udp::UdpBackend;
use crate::backend::vxlan::Vxlan;
use crate::subnet::SubnetManager;

pub struct BackendManager {
    pub subnet_manager: SubnetManager,
    pub ext_iface: ExternalInterface,
}

impl BackendManager {
    pub fn new(subnet_manager: SubnetManager, ext_iface: ExternalInterface) -> Self {
        Self {
            subnet_manager,
            ext_iface,
        }
    }
    pub fn get_backend(self, backend_type: &str) -> anyhow::Result<Box<dyn Backend + Send>> {
        match backend_type.to_lowercase().as_str() {
            "vxlan" => {
                let backend = Vxlan::new(self.subnet_manager, self.ext_iface);
                Ok(Box::new(backend))
            }
            "udp" => {
                let backend = UdpBackend::new(self.subnet_manager, self.ext_iface);
                Ok(Box::new(backend))
            }
            _ => {
                Err(anyhow::anyhow!("backend is not defined"))
            }
        }
    }
}