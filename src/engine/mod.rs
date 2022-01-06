use std::sync::Arc;

use anyhow::{Context, Result};

use self::metrics::*;
use self::ton_subscriber::*;
use crate::config::*;

mod metrics;
mod ton_subscriber;

pub struct Engine {
    _exporter: Arc<pomfrit::MetricsExporter>,
    _ton_subscriber: Arc<TonSubscriber>,
    ton_engine: Arc<ton_indexer::Engine>,
}

impl Engine {
    pub async fn new(config: AppConfig, global_config: ton_indexer::GlobalConfig) -> Result<Self> {
        let metrics_state = Arc::new(MetricsState::default());

        let (exporter, writer) = pomfrit::create_exporter(Some(config.metrics_settings)).await?;

        writer.spawn({
            let metrics_state = metrics_state.clone();
            move |buf| {
                buf.write(&metrics_state);
            }
        });

        let ton_subscriber = TonSubscriber::new(metrics_state.clone());
        let ton_engine = ton_indexer::Engine::new(
            config
                .node_settings
                .build_indexer_config()
                .await
                .context("Failed to build node config")?,
            global_config,
            vec![ton_subscriber.clone() as Arc<dyn ton_indexer::Subscriber>],
        )
        .await
        .context("Failed to start TON node")?;

        *metrics_state.engine_metrics.lock() = Some(ton_engine.metrics().clone());

        Ok(Self {
            _exporter: exporter,
            _ton_subscriber: ton_subscriber,
            ton_engine,
        })
    }

    pub async fn start(&self) -> Result<()> {
        self.ton_engine.start().await?;
        Ok(())
    }
}
