use anyhow::{Context, Result};
use everscale_monitoring::config::*;
use everscale_monitoring::engine::*;
use is_terminal::IsTerminal;
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: broxus_util::alloc::Allocator = broxus_util::alloc::allocator();

#[tokio::main]
async fn main() -> Result<()> {
    let logger = tracing_subscriber::fmt().with_env_filter(
        EnvFilter::builder()
            .with_default_directive(tracing::Level::INFO.into())
            .from_env_lossy(),
    );
    if std::io::stdout().is_terminal() {
        logger.init();
    } else {
        logger.without_time().init();
    }

    let app: App = argh::from_env();
    match app.command {
        Subcommand::Run(run) => run.execute().await,
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
    Run(CmdRun),
}

#[derive(argh::FromArgs)]
/// Starts service
#[argh(subcommand, name = "run")]
struct CmdRun {
    /// path to config file ('config.yaml' by default)
    #[argh(option, short = 'c', default = "String::from(\"config.yaml\")")]
    config: String,

    /// path to global config file
    #[argh(option, short = 'g')]
    global_config: String,
}

impl CmdRun {
    async fn execute(self) -> Result<()> {
        let config: AppConfig = broxus_util::read_config(&self.config)?;

        let global_config = ton_indexer::GlobalConfig::load(&self.global_config)
            .context("Failed to open global config")?;

        // Start listening termination signals
        let signal_rx = broxus_util::any_signal(broxus_util::TERMINATION_SIGNALS);

        let engine_fut = async {
            let engine = Engine::new(config, global_config)
                .await
                .context("Failed to create engine")?;
            engine.start().await.context("Failed to start engine")?;

            futures::future::pending().await
        };

        // Cancellable main loop
        tokio::select! {
            result = engine_fut => result,
            signal = signal_rx => {
                if let Ok(signal) = signal {
                    tracing::warn!(?signal, "received termination signal, flushing state...");
                }
                Ok(())
            },
        }
    }
}
