use anyhow::{Context, Result};
use everscale_monitoring::config::*;
use everscale_monitoring::engine::*;

#[global_allocator]
static GLOBAL: broxus_util::alloc::Allocator = broxus_util::alloc::allocator();

#[tokio::main]
async fn main() -> Result<()> {
    if atty::is(atty::Stream::Stdout) {
        tracing_subscriber::fmt::init();
    } else {
        tracing_subscriber::fmt::fmt().without_time().init();
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

        // Spawn cancellation future
        let cancellation_token = CancellationToken::new();
        let cancelled = cancellation_token.cancelled();

        tokio::spawn({
            let cancellation_token = cancellation_token.clone();

            async move {
                if let Ok(signal) = signal_rx.await {
                    tracing::warn!(?signal, "received termination signal");
                    cancellation_token.cancel();
                }
            }
        });

        let engine_fut = async {
            let engine = Engine::new(config, global_config)
                .await
                .context("Failed to create engine")?;
            engine.start().await.context("Failed to start engine")?;

            futures::future::pending().await
        };

        // Cancellable main loop
        tokio::select! {
            res = engine_fut => res,
            _ = cancelled => Ok(()),
        }
    }
}
