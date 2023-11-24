use nix::libc;
use std::convert::From;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

pub struct TunIo(RawFd);

impl From<RawFd> for TunIo {
    fn from(fd: RawFd) -> Self {
        Self(fd)
    }
}

impl FromRawFd for TunIo {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self(fd)
    }
}

impl AsRawFd for TunIo {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Read for TunIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = unsafe { libc::read(self.0, buf.as_ptr() as *mut _, buf.len() as _) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(n as _)
    }

    fn read_vectored(&mut self, bufs: &mut [std::io::IoSliceMut<'_>]) -> std::io::Result<usize> {
        unsafe {
            let iov = bufs.as_ptr().cast();
            let iovcnt = bufs.len().min(libc::c_int::MAX as usize) as _;

            let n = libc::readv(self.0, iov, iovcnt);
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(n as usize)
        }
    }
}

impl Write for TunIo {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        unsafe {
            let amount = libc::write(self.0, buf.as_ptr() as *const _, buf.len());

            if amount < 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(amount as usize)
        }
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        unsafe {
            let iov = bufs.as_ptr().cast();
            let iovcnt = bufs.len().min(libc::c_int::MAX as usize) as _;

            let n = libc::writev(self.0, iov, iovcnt);
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for TunIo {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}
