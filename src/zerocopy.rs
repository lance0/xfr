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
        let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
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

    /// One non-blocking `sendfile(2)` of the payload from offset 0.
    /// Partial sends are fine — the caller counts whatever was queued,
    /// matching `write(2)` semantics on the regular path.
    #[cfg(target_os = "linux")]
    fn sendfile_once(&self, socket_fd: std::os::unix::io::RawFd) -> io::Result<usize> {
        use std::os::unix::io::AsRawFd;

        let mut offset: libc::off_t = 0;
        let n = unsafe { libc::sendfile(socket_fd, self.file.as_raw_fd(), &mut offset, self.len) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Send one payload chunk on `stream` via sendfile, awaiting socket
    /// writability. Cancel-safe: dropping the future mid-await sends
    /// nothing; a completed sendfile returns synchronously.
    #[cfg(target_os = "linux")]
    pub async fn send_chunk(&self, stream: &TcpStream) -> io::Result<usize> {
        use std::os::unix::io::AsRawFd;
        use tokio::io::Interest;

        let socket_fd = stream.as_raw_fd();
        loop {
            stream.writable().await?;
            match stream.try_io(Interest::WRITABLE, || self.sendfile_once(socket_fd)) {
                Ok(n) => return Ok(n),
                // Spurious readiness or send buffer refilled between
                // writable() and sendfile — wait again.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn send_chunk(&self, _stream: &TcpStream) -> io::Result<usize> {
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
        while queued < pattern.len() * 2 {
            queued += payload.send_chunk(&sender).await.unwrap();
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
            while queued < TARGET {
                queued += payload.send_chunk(&sender).await.unwrap();
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
