//! The one iroh endpoint of this device. It dials the server (control ALPN) and
//! accepts/dials peers (media ALPN). Also converts between iroh addresses and the
//! `PeerAddr` the server relays between devices.

use crate::error::{net_err, Result};
use iroh::endpoint::{presets, IdleTimeout, QuicTransportConfig, VarInt};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, TransportAddr, Watcher};
use proto::control::{PeerAddr, PeerInfo};
use proto::DeviceId;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// Connection-level knobs shared by every connection this endpoint makes.
fn transport_config() -> Result<QuicTransportConfig> {
    let idle = IdleTimeout::try_from(Duration::from_secs(30)).map_err(net_err)?;
    Ok(QuicTransportConfig::builder()
        .max_idle_timeout(Some(idle))
        .keep_alive_interval(Duration::from_secs(5))
        // One uni stream per video frame: allow a deep backlog at 60 fps × families.
        .max_concurrent_uni_streams(VarInt::from_u32(4096))
        .max_concurrent_bidi_streams(VarInt::from_u32(64))
        .datagram_receive_buffer_size(Some(4 * 1024 * 1024))
        .datagram_send_buffer_size(4 * 1024 * 1024)
        .build())
}

#[derive(Clone)]
pub struct Net {
    endpoint: Endpoint,
}

impl std::fmt::Debug for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Net({})", self.endpoint.id().fmt_short())
    }
}

impl Net {
    /// Production endpoint: n0 relays and DNS address lookup, accepting media connections.
    pub async fn bind(secret: SecretKey, alpns: Vec<Vec<u8>>) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret)
            .alpns(alpns)
            .transport_config(transport_config()?)
            .bind()
            .await
            .map_err(net_err)?;
        Ok(Self { endpoint })
    }

    /// Endpoint for tests and the loopback harness: no relays, no lookup service,
    /// peers are dialed by their socket address on this machine.
    pub async fn bind_local(secret: SecretKey, alpns: Vec<Vec<u8>>) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .alpns(alpns)
            .relay_mode(RelayMode::Disabled)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .map_err(net_err)?
            .transport_config(transport_config()?)
            .bind()
            .await
            .map_err(net_err)?;
        Ok(Self { endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn id(&self) -> DeviceId {
        DeviceId(*self.endpoint.id().as_bytes())
    }

    /// Loopback-only address of this endpoint (for `bind_local` endpoints).
    pub fn local_addr(&self) -> EndpointAddr {
        let mut addr = EndpointAddr::new(self.endpoint.id());
        for sock in self.endpoint.bound_sockets() {
            let ip = if sock.ip().is_unspecified() {
                match sock.ip() {
                    IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                }
            } else {
                sock.ip()
            };
            addr = addr.with_ip_addr(SocketAddr::new(ip, sock.port()));
        }
        addr
    }

    /// What we tell the server about how to reach us.
    pub fn peer_addr(&self) -> PeerAddr {
        to_peer_addr(&self.endpoint.addr())
    }

    pub fn watch_addr(&self) -> impl Watcher<Value = EndpointAddr> + use<> {
        self.endpoint.watch_addr()
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

pub fn to_peer_addr(addr: &EndpointAddr) -> PeerAddr {
    PeerAddr {
        relay_url: addr.relay_urls().next().map(|u| u.to_string()),
        direct: addr.ip_addrs().copied().collect(),
    }
}

pub fn to_endpoint_addr(device: &DeviceId, addr: &PeerAddr) -> Result<EndpointAddr> {
    let id = crate::identity::device_id_to_endpoint(device)?;
    let mut out = EndpointAddr::new(id);
    if let Some(url) = &addr.relay_url {
        if let Ok(url) = url.parse::<RelayUrl>() {
            out = out.with_relay_url(url);
        }
    }
    out = out.with_addrs(addr.direct.iter().map(|a| TransportAddr::Ip(*a)));
    Ok(out)
}

pub fn peer_info_addr(peer: &PeerInfo) -> Result<EndpointAddr> {
    to_endpoint_addr(&peer.device_id, &peer.addr)
}

/// The server is dialed by key; a relay URL or direct address speeds that up.
pub fn server_addr(server: &DeviceId, addr: &PeerAddr) -> Result<EndpointAddr> {
    to_endpoint_addr(server, addr)
}
