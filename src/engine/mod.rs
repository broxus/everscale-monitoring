use std::sync::Arc;

use anyhow::{Context, Result};

use self::metrics::*;
use self::ton_subscriber::*;
use crate::config::*;

mod elector;
mod metrics;
mod ton_subscriber;

pub struct Engine {
    _exporter: Arc<pomfrit::MetricsExporter>,
    ton_subscriber: Arc<TonSubscriber>,
    ton_engine: Arc<ton_indexer::Engine>,
}

impl Engine {
    pub async fn new(config: AppConfig, global_config: ton_indexer::GlobalConfig) -> Result<Self> {
        // Create metrics state
        let metrics_state = Arc::new(MetricsState::default());

        // Create and spawn metrics exporter
        let (exporter, writer) = pomfrit::create_exporter(Some(config.metrics_settings)).await?;
        writer.spawn({
            let metrics_state = metrics_state.clone();
            move |buf| {
                buf.write(&metrics_state);
            }
        });

        let node_config = config
            .node_settings
            .build_indexer_config()
            .await
            .context("Failed tp build node config")?;

        // Create and sync TON node
        let ton_subscriber = TonSubscriber::new(metrics_state.clone());
        let ton_engine =
            ton_indexer::Engine::new(node_config, global_config, ton_subscriber.clone())
                .await
                .context("Failed to start TON node")?;

        // Set engine metrics object
        metrics_state.set_engine_ref(&ton_engine);

        // Done
        Ok(Self {
            _exporter: exporter,
            ton_subscriber,
            ton_engine,
        })
    }

    pub fn indexer(&self) -> &Arc<ton_indexer::Engine> {
        &self.ton_engine
    }

    pub async fn start(&self) -> Result<()> {
        self.ton_subscriber
            .start(&self.ton_engine)
            .await
            .context("Failed to init config metrics")?;

        self.ton_engine
            .start()
            .await
            .context("Failed to start TON node")?;

        Ok(())
    }
}
