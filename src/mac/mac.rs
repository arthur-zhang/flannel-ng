use macaddr::MacAddr6;
use rand::Rng;

pub fn new_hardware_addr() -> anyhow::Result<MacAddr6> {
    let mut rng = rand::thread_rng();
    let mut hardware_addr = [0u8; 6];
    rng.fill(&mut hardware_addr);
    hardware_addr[0] = (hardware_addr[0] & 0xfe) | 0x02;
    Ok(MacAddr6::from(hardware_addr))
}