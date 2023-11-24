use std::ffi::{CStr, CString};
use std::io::{IoSlice, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::anyhow;
use nix::libc;
use nix::libc::{c_char, fcntl, ifreq, F_GETFL, F_SETFL, IFF_NO_PI, IFF_TUN, O_NONBLOCK, O_RDWR};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::tun::io::TunIo;

nix::ioctl_write_int!(tunsetiff, b'T', 202);

#[macro_export]
macro_rules! ready {
    ($e:expr $(,)?) => {
        match $e {
            std::task::Poll::Ready(t) => t,
            std::task::Poll::Pending => return std::task::Poll::Pending,
        }
    };
}

pub async fn open_tun(name: &str) -> anyhow::Result<(RawFd, String)> {
    let tun_fd = unsafe { libc::open(b"/dev/net/tun\0".as_ptr() as *const _, O_RDWR) } as RawFd;

    let mut req: ifreq = unsafe { std::mem::zeroed() };
    let name = CString::new(name)?;
    if name.as_bytes_with_nul().len() > libc::IFNAMSIZ {
        panic!("dev name too long");
    }
    let ptr = name.as_ptr() as *const c_char;
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, req.ifr_name.as_mut_ptr(), name.as_bytes().len());
    }

    // The IFF_NO_PI flag, when set for a TUN device,
    // instructs the interface to omit the additional packet information header,
    // allowing direct read and write of raw IP packets.
    req.ifr_ifru.ifru_flags = (IFF_TUN | IFF_NO_PI) as i16;

    unsafe {
        tunsetiff(tun_fd, &req as *const _ as _)?;
    }
    let device_name = CStr::from_bytes_until_nul(&req.ifr_name)?.to_str()?;

    let res = set_nonblock(tun_fd);
    println!("set nonblock: {:?}", res);

    Ok((tun_fd, device_name.to_string()))
}

pub fn set_nonblock(fd: RawFd) -> anyhow::Result<()> {
    match unsafe { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) } {
        0 => Ok(()),
        _ => Err(anyhow!(std::io::Error::last_os_error())),
    }
}

pub struct Tun {
    pub inner: AsyncFd<TunIo>,
}

impl Tun {
    pub fn new(fd: RawFd) -> anyhow::Result<Self> {
        unsafe {
            Ok(Self {
                inner: AsyncFd::new(TunIo::from_raw_fd(fd))?,
            })
        }
    }
}

impl AsyncRead for Tun {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = ready!(self.inner.poll_read_ready_mut(cx))?;
            let rbuf = buf.initialize_unfilled();
            match guard.try_io(|inner| inner.get_mut().read(rbuf)) {
                Ok(res) => return Poll::Ready(res.map(|n| buf.advance(n))),
                Err(_wb) => continue,
            }
        }
    }
}

impl AsyncWrite for Tun {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            let mut guard = ready!(self.inner.poll_write_ready_mut(cx))?;
            match guard.try_io(|inner| inner.get_mut().write(buf)) {
                Ok(res) => return Poll::Ready(res),
                Err(_wb) => continue,
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = ready!(self.inner.poll_write_ready_mut(cx))?;
            match guard.try_io(|inner| inner.get_mut().flush()) {
                Ok(res) => return Poll::Ready(res),
                Err(_wb) => continue,
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        loop {
            let mut guard = ready!(self.inner.poll_write_ready_mut(cx))?;
            match guard.try_io(|inner| inner.get_mut().write_vectored(bufs)) {
                Ok(res) => return Poll::Ready(res),
                Err(_wb) => continue,
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }
}
