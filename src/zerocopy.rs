//! Zero-copy TCP send support (issue #33)
//!
//! Implements iperf3-style `--zerocopy`: the payload lives in an anonymous
//! memory file (`memfd_create`) and is pushed to the socket with
//! `sendfile(2)`, skipping the userspace-to-kernel copy that `write(2)`
//! performs on every chunk. On constrained CPUs (embedded routers) the copy
//! itself is often the throughput bottleneck, so eliding it raises the
//! ceiling and lowers CPU load at any given rate.
//!
//! `sendfile` is used rather than `MSG_ZEROCOPY` because it works on old
//! kernels (the embedded systems motivating this feature), needs no
//! error-queue completion reaping, and is supported by MPTCP.
//!
//! Linux-only. On other platforms [`ZerocopyPayload::new`] returns
//! `ErrorKind::Unsupported` and callers fall back to regular writes.

use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
use tokio::net::TcpStream;

/// A fixed payload backed by a memfd, sent repeatedly with `sendfile(2)`.
///
/// The payload contents are written once at construction (random or zeros,
/// matching the regular send path) and never change, so every
/// [`send_chunk`](Self::send_chunk) transmits the same bytes — the same
/// semantics as the regular path's reused userspace buffer.
#[derive(Debug)]
pub struct ZerocopyPayload {
    #[cfg(target_os = "linux")]
    file: std::fs::File,
    len: usize,
}

impl ZerocopyPayload {
    /// Create a payload fd containing `payload`.
    ///
    /// Fails with `ErrorKind::Unsupported` on non-Linux platforms or when
    /// `memfd_create` is unavailable (kernel < 3.17, seccomp); callers
    /// should fall back to regular writes.
    #[cfg(target_os = "linux")]
    pub fn new(payload: &[u8]) -> io::Result<Self> {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        if payload.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero-copy payload must not be empty",
            ));
        }

        // CStr literal avoids a CString allocation; name is debug-only
        // (shows up in /proc/<pid>/fd).
        let name = c"xfr-zerocopy";
        // Raw syscall rather than libc::memfd_create: the glibc wrapper only
        // exists since 2.27, and both cross-compile toolchains and the old
        // embedded systems this feature targets ship older glibc. The kernel
        // interface (3.17+) is the actual requirement either way.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr(),
                libc::MFD_CLOEXEC as libc::c_uint,
            )
        } as libc::c_int;
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(payload)?;

        Ok(Self {
            file,
            len: payload.len(),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new(_payload: &[u8]) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "zero-copy sends require Linux sendfile(2)",
        ))
    }

    /// Payload size in bytes (the chunk size of each `send_chunk`).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// One non-blocking `sendfile(2)` call starting from `offset`.
    /// Returns the number of bytes queued by this call.
    #[cfg(target_os = "linux")]
    fn sendfile_once(
        &self,
        socket_fd: std::os::unix::io::RawFd,
        offset: usize,
    ) -> io::Result<usize> {
        let remaining = self.len.saturating_sub(offset);
        if remaining == 0 {
            return Ok(0);
        }
        let mut off = offset as libc::off_t;
        let n = unsafe { libc::sendfile(socket_fd, self.file.as_raw_fd(), &mut off, remaining) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Send one payload slice starting from `offset` on `stream` via sendfile,
    /// awaiting socket writability. Returns the short count queued by the
    /// kernel; the caller owns the offset and resumes from `offset + n`,
    /// mirroring the regular `try_send` path.
    #[cfg(target_os = "linux")]
    pub async fn send_chunk(&self, stream: &TcpStream, offset: usize) -> io::Result<usize> {
        use tokio::io::Interest;

        let socket_fd = stream.as_raw_fd();
        stream.writable().await?;
        match stream.try_io(Interest::WRITABLE, || self.sendfile_once(socket_fd, offset)) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn send_chunk(&self, _stream: &TcpStream, _offset: usize) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "zero-copy sends require Linux sendfile(2)",
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn new_rejects_empty_payload() {
        let err = ZerocopyPayload::new(&[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn new_creates_payload_of_requested_len() {
        let payload = ZerocopyPayload::new(&[0xAB; 4096]).unwrap();
        assert_eq!(payload.len(), 4096);
        assert!(!payload.is_empty());
    }

    /// sendfile path delivers exactly the payload bytes over a real TCP
    /// loopback connection, repeatedly, from a stable offset-0 chunk.
    #[tokio::test]
    async fn send_chunk_delivers_payload_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pattern: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let payload = ZerocopyPayload::new(&pattern).unwrap();

        let sender = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut receiver, _) = listener.accept().await.unwrap();

        // Send the chunk twice to verify the offset resets per call.
        let mut queued = 0usize;
        let mut chunk_offset = 0usize;
        while queued < pattern.len() * 2 {
            let n = payload.send_chunk(&sender, chunk_offset).await.unwrap();
            chunk_offset += n;
            queued += n;
            if chunk_offset >= pattern.len() {
                chunk_offset = 0;
            }
        }
        drop(sender);

        let mut received = Vec::new();
        receiver.read_to_end(&mut received).await.unwrap();
        assert_eq!(received.len(), pattern.len() * 2);
        assert_eq!(&received[..pattern.len()], pattern.as_slice());
        assert_eq!(&received[pattern.len()..], pattern.as_slice());
    }

    /// EAGAIN handling: a small payload sent in a tight loop against a
    /// receiver that drains slowly must eventually push every byte through
    /// the writable()/WouldBlock retry path without error.
    #[tokio::test]
    async fn send_chunk_handles_full_send_buffer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pattern = vec![0x5A; 64 * 1024];
        let payload = ZerocopyPayload::new(&pattern).unwrap();

        let sender = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut receiver, _) = listener.accept().await.unwrap();

        const TARGET: usize = 8 * 1024 * 1024; // well past any send buffer
        let send_task = tokio::spawn(async move {
            let mut queued = 0usize;
            let mut chunk_offset = 0usize;
            while queued < TARGET {
                let n = payload.send_chunk(&sender, chunk_offset).await.unwrap();
                chunk_offset += n;
                queued += n;
                if chunk_offset >= pattern.len() {
                    chunk_offset = 0;
                }
            }
            queued
        });

        let mut drained = 0usize;
        let mut buf = vec![0u8; 256 * 1024];
        let sent = loop {
            if send_task.is_finished() && drained > 0 {
                // Sender done; drain whatever remains in flight.
                let sent = send_task.await.unwrap();
                while drained < sent {
                    drained += receiver.read(&mut buf).await.unwrap();
                }
                break sent;
            }
            drained += receiver.read(&mut buf).await.unwrap();
        };
        assert!(sent >= TARGET);
        assert!(drained >= sent);
    }
}
