use std::net::{Ipv4Addr, SocketAddrV4};

use anyhow::{anyhow, Context, Result};
use global_config::*;

#[tokio::main]
async fn main() {
    env_logger::init();

    let app: App = argh::from_env();
    if let Err(e) = app.execute().await {
        eprintln!("{:?}", e);
        std::process::exit(1);
    }
}

#[derive(argh::FromArgs)]
#[argh(description = "")]
struct App {
    /// public ip address (resolved automatically if not specified)
    #[argh(option)]
    ip: Option<Ipv4Addr>,

    /// ADNL UDP port
    #[argh(option, short = 'c', default = "30304")]
    port: u16,

    /// path to global config file
    #[argh(option, short = 'g')]
    global_config: String,
}

impl App {
    async fn execute(self) -> Result<()> {
        let global_config =
            GlobalConfig::load(self.global_config).context("Failed to load global config")?;

        let ip_address = match self.ip {
            Some(address) => address,
            None => public_ip::addr_v4()
                .await
                .ok_or(anyhow!("Failed to find public ip"))?,
        };

        let nodes =
            geoip_resolver::search_nodes(SocketAddrV4::new(ip_address, self.port), global_config)
                .await
                .context("Failed to search nodes")?;
    }
}
