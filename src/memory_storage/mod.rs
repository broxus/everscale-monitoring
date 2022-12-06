use std::net::SocketAddrV4;
use std::sync::Arc;
use ton_types::FxDashMap;

pub struct MemoryStorage {
    nodes: FxDashMap<[u8; 32], Option<SocketAddrV4>>,
}

impl MemoryStorage {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            nodes: FxDashMap::default(),
        })
    }

    pub fn get(&self, adnl: &[u8; 32]) -> Option<SocketAddrV4> {
        self.nodes.get(adnl).and_then(|x| *x)
    }

    pub fn insert_or_update_node(&self, adnl: &[u8; 32], node_ip: Option<SocketAddrV4>) {
        if let Some(mut address) = self.nodes.get_mut(adnl) {
            if address.is_none() {
                *address = node_ip
            }
        } else {
            self.nodes.insert(*adnl, node_ip);
        }
    }
}
