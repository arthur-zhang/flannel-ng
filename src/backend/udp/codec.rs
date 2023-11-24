use anyhow::bail;

pub struct IpSlice<'a> {
    slice: &'a mut [u8],
}

const SERIALIZED_SIZE: usize = 20;

impl<'a> IpSlice<'a> {
    pub fn new(slice: &'a mut [u8]) -> anyhow::Result<Self> {
        if slice.len() < SERIALIZED_SIZE {
            bail!(
                "ip slice length should be at least {} bytes",
                SERIALIZED_SIZE
            );
        }

        Ok(IpSlice { slice })
    }
    pub fn version(&self) -> u8 {
        unsafe { *self.slice.get_unchecked(0) >> 4 }
    }
    #[allow(dead_code)]
    pub fn ttl(&self) -> u8 {
        unsafe { *self.slice.get_unchecked(8) }
    }
    pub fn ttl_mut(&mut self) -> &mut u8 {
        unsafe { self.slice.as_mut().get_unchecked_mut(8) }
    }
    #[inline]
    pub fn destination(&self) -> [u8; 4] {
        // SAFETY:
        // Safe as the slice length is checked to be at least
        // SERIALIZED_SIZE (20) in the constructor.
        unsafe { get_unchecked_4_byte_array(self.slice.as_ptr().add(16)) }
    }
}
#[inline]
unsafe fn get_unchecked_4_byte_array(ptr: *const u8) -> [u8; 4] {
    [*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]
}

#[cfg(test)]
mod tests {
    pub use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_ip_v4_packet() {
        let mut packet_bytes: [u8; 84] = [
            0x45, 0x00, 0x00, 0x54, 0x52, 0x83, 0x40, 0x00, 0x3f, 0x01, 0x61, 0x01, 0xac, 0x11,
            0x2b, 0x00, 0xac, 0x11, 0x05, 0x02, 0x08, 0x00, 0x48, 0xef, 0x63, 0x8a, 0x14, 0x83,
            0x17, 0x25, 0x5c, 0x65, 0x00, 0x00, 0x00, 0x00, 0xf6, 0xa5, 0x0e, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29,
            0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
        ];

        let mut ip_slice = IpSlice {
            slice: &mut packet_bytes,
        };

        let version = ip_slice.version();
        println!("version: {}", version);
        assert_eq!(version, 4);
        let ttl = ip_slice.ttl();
        println!("ttl: {}", ttl);
        let ttl_mut = ip_slice.ttl_mut();
        *ttl_mut = 255;
        let ttl = ip_slice.ttl();
        println!("ttl: {}", ttl);
        let dst_addr = Ipv4Addr::from(ip_slice.destination());
        println!("dst_addr: {}", dst_addr);
    }
}
