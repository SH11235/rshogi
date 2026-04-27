//! CSAクライアント設定

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

/// CSAクライアント全体の設定
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct CsaClientConfig {
    pub server: ServerConfig,
    pub engine: EngineConfig,
    pub time: TimeConfig,
    pub game: GameConfig,
    pub retry: RetryConfig,
    pub record: RecordConfig,
    pub log: LogConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// 接続先。`host:port` 直書き、`tcp://host`、`ws://host[:port]/ws/<room>`、
    /// `wss://host[:port]/ws/<room>` のいずれか。`ws://` / `wss://` の場合は
    /// `port` 設定は無視される（URL に含めること）。
    pub host: String,
    pub port: u16,
    pub id: String,
    pub password: String,
    pub floodgate: bool,
    pub keepalive: KeepaliveConfig,
    /// WebSocket Upgrade 時に送る `Origin` ヘッダ値。`None` のとき
    /// `tungstenite` の既定値（URL から導出）に任せる。Cloudflare Workers の
    /// `WS_ALLOWED_ORIGINS` allowlist 通過のため、運用時は明示する。
    #[serde(default)]
    pub ws_origin: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "wdoor.c.u-tokyo.ac.jp".to_string(),
            port: 4081,
            id: String::new(),
            password: String::new(),
            floodgate: true,
            keepalive: KeepaliveConfig::default(),
            ws_origin: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct KeepaliveConfig {
    /// TCP SO_KEEPALIVE を有効化
    pub tcp: bool,
    /// CSAレベル空行ping間隔（秒）。0で無効
    pub ping_interval_sec: u64,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            tcp: true,
            ping_interval_sec: 60,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub path: PathBuf,
    pub startup_timeout_sec: u64,
    /// USIオプション (key → value)
    #[serde(default)]
    pub options: HashMap<String, toml::Value>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            startup_timeout_sec: 30,
            options: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TimeConfig {
    /// 秒読みマージン（ミリ秒）
    pub margin_msec: u64,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self { margin_msec: 2500 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct GameConfig {
    /// 最大対局数 (0 = 無制限)
    pub max_games: u32,
    /// 毎局エンジンを再起動するか
    pub restart_engine_every_game: bool,
    /// ponder を有効化
    pub ponder: bool,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            max_games: 0,
            restart_engine_every_game: false,
            ponder: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RetryConfig {
    pub initial_delay_sec: u64,
    pub max_delay_sec: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_delay_sec: 10,
            max_delay_sec: 900,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RecordConfig {
    pub enabled: bool,
    pub dir: PathBuf,
    pub filename_template: String,
    pub save_csa: bool,
    pub save_sfen: bool,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: PathBuf::from("./records"),
            filename_template: "{datetime}_{sente}_vs_{gote}".to_string(),
            save_csa: true,
            save_sfen: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub level: String,
    pub dir: PathBuf,
    pub stdout: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            dir: PathBuf::from("./logs"),
            stdout: true,
        }
    }
}

impl CsaClientConfig {
    /// TOML ファイルから設定を読み込む
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&text)?;
        Ok(config)
    }

    /// バリデーション
    pub fn validate(&self) -> Result<()> {
        if self.server.id.is_empty() {
            bail!("server.id is required");
        }
        if self.engine.path.as_os_str().is_empty() {
            bail!("engine.path is required");
        }
        if self.server.keepalive.ping_interval_sec > 0
            && self.server.keepalive.ping_interval_sec < 30
        {
            bail!("keepalive.ping_interval_sec must be >= 30 (CSA protocol requirement)");
        }
        Ok(())
    }
}
