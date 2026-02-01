//! TCP_INFO extraction for Linux and macOS
//!
//! Provides access to kernel-level TCP statistics like RTT, congestion window,
//! and retransmit counts.

use crate::protocol::TcpInfoSnapshot;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::mem;
    use std::os::unix::io::AsRawFd;

    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy)]
    pub struct TcpInfo {
        pub tcpi_state: u8,
        pub tcpi_ca_state: u8,
        pub tcpi_retransmits: u8,
        pub tcpi_probes: u8,
        pub tcpi_backoff: u8,
        pub tcpi_options: u8,
        pub tcpi_snd_wscale_rcv_wscale: u8,
        pub tcpi_delivery_rate_app_limited: u8,

        pub tcpi_rto: u32,
        pub tcpi_ato: u32,
        pub tcpi_snd_mss: u32,
        pub tcpi_rcv_mss: u32,

        pub tcpi_unacked: u32,
        pub tcpi_sacked: u32,
        pub tcpi_lost: u32,
        pub tcpi_retrans: u32,
        pub tcpi_fackets: u32,

        pub tcpi_last_data_sent: u32,
        pub tcpi_last_ack_sent: u32,
        pub tcpi_last_data_recv: u32,
        pub tcpi_last_ack_recv: u32,

        pub tcpi_pmtu: u32,
        pub tcpi_rcv_ssthresh: u32,
        pub tcpi_rtt: u32,
        pub tcpi_rttvar: u32,
        pub tcpi_snd_ssthresh: u32,
        pub tcpi_snd_cwnd: u32,
        pub tcpi_advmss: u32,
        pub tcpi_reordering: u32,

        pub tcpi_rcv_rtt: u32,
        pub tcpi_rcv_space: u32,

        pub tcpi_total_retrans: u32,
    }

    pub fn get_tcp_info<S: AsRawFd>(socket: &S) -> std::io::Result<TcpInfoSnapshot> {
        let fd = socket.as_raw_fd();
        let mut info: TcpInfo = Default::default();
        let mut len = mem::size_of::<TcpInfo>() as libc::socklen_t;

        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_INFO,
                &mut info as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };

        if ret == 0 {
            Ok(TcpInfoSnapshot {
                retransmits: info.tcpi_total_retrans as u64,
                rtt_us: info.tcpi_rtt,
                rtt_var_us: info.tcpi_rttvar,
                cwnd: info.tcpi_snd_cwnd,
            })
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::mem;
    use std::os::unix::io::AsRawFd;

    // macOS uses TCP_CONNECTION_INFO
    const TCP_CONNECTION_INFO: libc::c_int = 0x106;

    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy)]
    pub struct TcpConnectionInfo {
        pub tcpi_state: u8,
        pub tcpi_snd_wscale: u8,
        pub tcpi_rcv_wscale: u8,
        pub __pad1: u8,
        pub tcpi_options: u32,
        pub tcpi_flags: u32,
        pub tcpi_rto: u32,
        pub tcpi_maxseg: u32,
        pub tcpi_snd_ssthresh: u32,
        pub tcpi_snd_cwnd: u32,
        pub tcpi_snd_wnd: u32,
        pub tcpi_snd_sbbytes: u32,
        pub tcpi_rcv_wnd: u32,
        pub tcpi_rttcur: u32,
        pub tcpi_srtt: u32,
        pub tcpi_rttvar: u32,
        pub tcpi_tfo_cookie_req: u32,
        pub tcpi_tfo_cookie_rcv: u32,
        pub tcpi_tfo_syn_loss: u32,
        pub tcpi_tfo_syn_data_sent: u32,
        pub tcpi_tfo_syn_data_acked: u32,
        pub tcpi_tfo_syn_data_rcv: u32,
        pub tcpi_tfo_cookie_wrong: u32,
        pub tcpi_tfo_no_cookie_rcv: u32,
        pub tcpi_tfo_heuristics_disable: u32,
        pub tcpi_tfo_send_blackhole: u32,
        pub tcpi_tfo_recv_blackhole: u32,
        pub tcpi_tfo_onebyte_proxy: u32,
        pub tcpi_txpackets: u64,
        pub tcpi_txbytes: u64,
        pub tcpi_txretransmitbytes: u64,
        pub tcpi_rxpackets: u64,
        pub tcpi_rxbytes: u64,
        pub tcpi_rxoutoforderbytes: u64,
        pub tcpi_txretransmitpackets: u64,
    }

    pub fn get_tcp_info<S: AsRawFd>(socket: &S) -> std::io::Result<TcpInfoSnapshot> {
        let fd = socket.as_raw_fd();
        let mut info: TcpConnectionInfo = Default::default();
        let mut len = mem::size_of::<TcpConnectionInfo>() as libc::socklen_t;

        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_TCP,
                TCP_CONNECTION_INFO,
                &mut info as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };

        if ret == 0 {
            Ok(TcpInfoSnapshot {
                retransmits: info.tcpi_txretransmitpackets,
                rtt_us: info.tcpi_srtt,
                rtt_var_us: info.tcpi_rttvar,
                cwnd: info.tcpi_snd_cwnd,
            })
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod fallback {
    use super::*;

    pub fn get_tcp_info<S>(_socket: &S) -> std::io::Result<TcpInfoSnapshot> {
        // Return empty stats on unsupported platforms
        Ok(TcpInfoSnapshot {
            retransmits: 0,
            rtt_us: 0,
            rtt_var_us: 0,
            cwnd: 0,
        })
    }
}

#[cfg(target_os = "linux")]
pub use linux::get_tcp_info;

#[cfg(target_os = "macos")]
pub use macos::get_tcp_info;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use fallback::get_tcp_info;
