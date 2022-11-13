use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{ExitCode, Termination};

use anyhow::{Context, Result};
use geoip_resolver::*;
use global_config::GlobalConfig;

#[global_allocator]
static GLOBAL: broxus_util::alloc::Allocator = broxus_util::alloc::allocator();

#[tokio::main]
async fn main() -> impl Termination {
    tracing_subscriber::fmt::init();

    let app: App = argh::from_env();
    if let Err(e) = match app.command {
        Subcommand::Resolve(resolve) => resolve.execute().await,
        Subcommand::Import(import) => import.execute(),
    } {
        eprintln!("{:?}", e);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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
    #[argh(option, short = 'c', default = "30305")]
    port: u16,

    /// path to global config file
    #[argh(option, short = 'g')]
    global_config: String,

    /// path to the resolver DB
    #[argh(option)]
    db: PathBuf,

    /// path to the output file
    #[argh(positional)]
    output: PathBuf,
}

impl CmdResolve {
    async fn execute(self) -> Result<()> {
        let mut temp_extension = self.output.extension().unwrap_or_default().to_os_string();
        temp_extension.push(std::ffi::OsString::from("temp"));

        let mut temp_file_path = self.output.clone();
        temp_file_path.set_extension(temp_extension);

        let db = GeoDataReader::new(self.db)?;

        let global_config =
            GlobalConfig::load(self.global_config).context("Failed to load global config")?;

        let ip_address = broxus_util::resolve_public_ip(self.ip).await?;

        let nodes = search_nodes(SocketAddrV4::new(ip_address, self.port), global_config)
            .await
            .context("Failed to search nodes")?;

        let mut temp_file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .mode(0o644)
            .open(&temp_file_path)?;

        db.with_cfs(|mut resolver| {
            for node in nodes {
                let info = resolver.find(node.into())?;
                write!(temp_file, "{}", info)?;
            }
            Ok(())
        })?;

        drop(temp_file);
        std::fs::rename(temp_file_path, self.output)?;

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
