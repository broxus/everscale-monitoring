use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use geoip_resolver::*;
use global_config::*;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() {
    env_logger::init();

    let app: App = argh::from_env();
    if let Err(e) = match app.command {
        Subcommand::Resolve(resolve) => resolve.execute().await,
        Subcommand::Import(import) => import.execute(),
    } {
        eprintln!("{:?}", e);
        std::process::exit(1);
    }
}

#[derive(argh::FromArgs)]
#[argh(description = "")]
struct App {
    #[argh(subcommand)]
    command: Subcommand,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum Subcommand {
    Resolve(CmdResolve),
    Import(CmdImport),
}

#[derive(argh::FromArgs)]
/// Searches all peers and info about them
#[argh(subcommand, name = "resolve")]
struct CmdResolve {
    /// public ip address (resolved automatically if not specified)
    #[argh(option)]
    ip: Option<Ipv4Addr>,

    /// ADNL UDP port
    #[argh(option, short = 'c', default = "30304")]
    port: u16,

    /// path to global config file
    #[argh(option, short = 'g')]
    global_config: String,

    /// path to the resolver DB
    #[argh(option)]
    db: PathBuf,
}

impl CmdResolve {
    async fn execute(self) -> Result<()> {
        let db = GeoDataReader::new(self.db)?;

        let global_config =
            GlobalConfig::load(self.global_config).context("Failed to load global config")?;

        let ip_address = match self.ip {
            Some(address) => address,
            None => public_ip::addr_v4()
                .await
                .ok_or(anyhow!("Failed to find public ip"))?,
        };

        let nodes = search_nodes(SocketAddrV4::new(ip_address, self.port), global_config)
            .await
            .context("Failed to search nodes")?;

        db.with_cfs(|mut resolver| {
            for node in nodes {
                let info = resolver.find(node.into())?;
                print!("{}", info);
            }

            Ok(())
        })?;

        Ok(())
    }
}

#[derive(argh::FromArgs)]
/// Imports IP2Location database
#[argh(subcommand, name = "import")]
struct CmdImport {
    /// path to the resolver DB
    #[argh(option)]
    db: PathBuf,

    /// path to the ASN csv file
    #[argh(option)]
    asn: Option<PathBuf>,

    /// path to the location csv file
    #[argh(option)]
    locations: Option<PathBuf>,
}

impl CmdImport {
    fn execute(self) -> Result<()> {
        let mut geo_data = GeoDataImporter::new(self.db)?;

        if let Some(asn) = self.asn {
            geo_data
                .import_asn(asn)
                .context("Failed to import ASN data")?;
        }

        if let Some(locations) = self.locations {
            geo_data
                .import_locations(locations)
                .context("Failed to import locations")?;
        }

        Ok(())
    }
}
