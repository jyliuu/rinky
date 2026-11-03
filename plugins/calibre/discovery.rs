//! Finding calibre on the local network: a UDP datagram to each of calibre's broadcast
//! ports; calibre answers `calibre wireless device client (on HOST);OPDS_PORT,PORT` to
//! the sender (KOReader `find_calibre_server`).

use crate::protocol;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// calibre's broadcast ports.
pub const BROADCAST_PORTS: &[u16] = &[54982, 48123, 39001, 44044, 59678];

/// A calibre found on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// calibre's host name (from its answer).
    pub name: String,
    /// Address to connect to.
    pub addr: SocketAddr,
}

/// The broadcast targets: 255.255.255.255 on each of [`BROADCAST_PORTS`].
pub fn broadcast_targets() -> Vec<SocketAddr> {
    BROADCAST_PORTS.iter().map(|p| SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), *p)).collect()
}

/// Sends `hello` to each target in turn and waits up to `wait` for an answer. Returns
/// the first calibre that answers, `Ok(None)` when none does, or the send error when no
/// datagram could be sent at all (Wi-Fi off: "network is unreachable").
pub fn discover(targets: &[SocketAddr], wait: Duration, cancelled: &dyn Fn() -> bool) -> std::io::Result<Option<Found>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(250)))?;
    let mut sent_any = None;
    for t in targets {
        if cancelled() {
            return Ok(None);
        }
        if let Err(e) = sock.send_to(b"hello", t) {
            log::info!("calibre: discovery to {t}: {e}");
            sent_any.get_or_insert(Err(e));
            continue;
        }
        sent_any = Some(Ok(()));
        let until = Instant::now() + wait;
        let mut buf = [0u8; 512];
        while Instant::now() < until && !cancelled() {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    match protocol::parse_discovery(&text) {
                        Some((name, port)) => {
                            log::info!("calibre: found {name:?} at {}:{port} (answer to {t})", from.ip());
                            return Ok(Some(Found { name, addr: SocketAddr::new(from.ip(), port) }));
                        }
                        None => log::info!("calibre: ignored datagram from {from}: {text:?}"),
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => {
                    log::info!("calibre: discovery receive: {e}");
                    break;
                }
            }
        }
    }
    match sent_any {
        Some(Err(e)) => Err(e),
        _ => Ok(None),
    }
}
