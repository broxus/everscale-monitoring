use anyhow::{Context, Result};
use everscale_monitoring::config::*;
use everscale_monitoring::engine::*;

#[global_allocator]
static GLOBAL: ton_indexer::alloc::Allocator = ton_indexer::alloc::allocator();

#[tokio::main]
async fn main() -> Result<()> {
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
        let config: AppConfig = read_config(&self.config)?;
        let _logger = init_logger(&config.logger_settings).context("Failed to init logger")?;

        let global_config = ton_indexer::GlobalConfig::load(&self.global_config)
            .context("Failed to open global config")?;

        let engine = Engine::new(config, global_config)
            .await
            .context("Failed to create engine")?;
        engine.start().await.context("Failed to start engine")?;

        futures::future::pending().await
    }
}

pub fn read_config<P, T>(path: P) -> Result<T>
where
    P: AsRef<std::path::Path>,
    for<'de> T: serde::Deserialize<'de>,
{
    let data = std::fs::read_to_string(path).context("Failed to read config")?;
    let re = regex::Regex::new(r"\$\{([a-zA-Z_][0-9a-zA-Z_]*)\}").unwrap();
    let result = re.replace_all(&data, |caps: &regex::Captures| {
        match std::env::var(&caps[1]) {
            Ok(value) => value,
            Err(_) => {
                eprintln!("WARN: Environment variable {} was not set", &caps[1]);
                String::default()
            }
        }
    });

    config::Config::builder()
        .add_source(config::File::from_str(
            result.as_ref(),
            config::FileFormat::Yaml,
        ))
        .build()
        .context("Failed to build config")?
        .try_deserialize()
        .context("Failed to parse config")
}

pub fn init_logger(initial_value: &serde_yaml::Value) -> Result<log4rs::Handle> {
    let handle = log4rs::config::init_config(parse_logger_config(initial_value.clone())?)?;
    Ok(handle)
}

pub fn parse_logger_config(value: serde_yaml::Value) -> Result<log4rs::Config> {
    let config = serde_yaml::from_value::<log4rs::config::RawConfig>(value)?;

    let (appenders, errors) = config.appenders_lossy(&log4rs::config::Deserializers::default());
    if !errors.is_empty() {
        return Err(anyhow::Error::msg(
            "Errors found when deserializing the logger config",
        ))
        .with_context(|| format!("{:#?}", errors));
    }

    let config = log4rs::Config::builder()
        .appenders(appenders)
        .loggers(config.loggers())
        .build(config.root())?;
    Ok(config)
}
