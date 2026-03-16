use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub workers: WorkerConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_jobs: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: default_max_concurrent(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    #[serde(default = "default_working_dir")]
    pub default_working_dir: String,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    #[serde(default = "default_kill_grace")]
    pub kill_grace_period_secs: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            default_working_dir: default_working_dir(),
            log_dir: default_log_dir(),
            kill_grace_period_secs: default_kill_grace(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashboardConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub static_dir: Option<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            static_dir: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_keys: Vec<ApiKeyEntry>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_keys: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyEntry {
    pub name: String,
    pub key: String,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    9400
}
fn default_max_concurrent() -> usize {
    8
}
fn default_working_dir() -> String {
    "/opt/wartable/jobs".to_string()
}
fn default_log_dir() -> String {
    "/opt/wartable/logs".to_string()
}
fn default_kill_grace() -> u64 {
    10
}
fn default_true() -> bool {
    true
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config {
                server: ServerConfig::default(),
                scheduler: SchedulerConfig::default(),
                workers: WorkerConfig::default(),
                dashboard: DashboardConfig::default(),
                auth: AuthConfig::default(),
            })
        }
    }

    pub fn config_path() -> PathBuf {
        let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        p.push(".wartable/config.toml");
        p
    }

    pub fn log_dir(&self) -> PathBuf {
        let expanded = shellexpand_tilde(&self.workers.log_dir);
        PathBuf::from(expanded)
    }

    pub fn working_dir(&self) -> PathBuf {
        let expanded = shellexpand_tilde(&self.workers.default_working_dir);
        PathBuf::from(expanded)
    }
}

fn shellexpand_tilde(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        if let Some(home) = dirs::home_dir() {
            return path.replacen("~", &home.to_string_lossy(), 1);
        }
    }
    path.to_string()
}
