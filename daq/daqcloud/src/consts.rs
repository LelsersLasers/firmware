use std::net;

pub const DATABASE_URL: &str = "cloud_registry.sqlite";
pub const STORE_DIR: &str = "cloud_store";
pub const BIND_ADDR: net::SocketAddr =
    net::SocketAddr::V4(net::SocketAddrV4::new(net::Ipv4Addr::UNSPECIFIED, 3000));
/// Default combined in-memory cache budget (tiles + partition preloads) in MiB.
pub const DEFAULT_MAX_CACHE_MB: usize = 64;
