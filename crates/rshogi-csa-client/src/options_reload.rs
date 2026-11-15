use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{Cli, CsaClientConfig, apply_cli_overrides};

type Options = HashMap<String, toml::Value>;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
}

impl FileStamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).with_context(|| {
            format!("設定ファイルの metadata を取得できません: {}", path.display())
        })?;
        Ok(Self {
            modified: metadata.modified().context("設定ファイルの更新時刻を取得できません")?,
            len: metadata.len(),
        })
    }
}

pub(super) struct OptionsReloader {
    path: PathBuf,
    stamp: FileStamp,
}

pub(super) struct OptionsUpdate {
    pub options: Options,
    pub changed: Options,
    stamp: FileStamp,
}

#[derive(Default, Deserialize)]
struct ReloadEngine {
    #[serde(default)]
    options: Options,
}

#[derive(Deserialize)]
struct ReloadConfig {
    #[serde(default)]
    engine: ReloadEngine,
}

impl OptionsReloader {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            stamp: FileStamp::read(path)?,
        })
    }

    pub fn prepare(&self, current: &Options, cli: &Cli) -> Result<Option<OptionsUpdate>> {
        let stamp = FileStamp::read(&self.path)?;
        if stamp == self.stamp {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&self.path).context("設定ファイルを読み直せません")?;
        let loaded: ReloadConfig = toml::from_str(&text).map_err(|error: toml::de::Error| {
            anyhow::anyhow!("TOML の解析失敗: {}", error.message())
        })?;
        if FileStamp::read(&self.path)? != stamp {
            bail!("読込中に設定ファイルが変更されました");
        }
        let mut overrides = CsaClientConfig::default();
        apply_cli_overrides(&mut overrides, cli);
        let mut options = loaded.engine.options;
        options.extend(overrides.engine.options);
        if let Some(name) = current.keys().filter(|name| !options.contains_key(*name)).min() {
            bail!("オプション {name} の削除は反映できません。戻す値を明示してください");
        }
        for (name, value) in &options {
            if name.contains(['\r', '\n']) {
                bail!("オプション名に改行は使えません");
            }
            match value {
                toml::Value::String(value) if value.contains(['\r', '\n']) => {
                    bail!("オプション {name} の値に改行は使えません");
                }
                toml::Value::String(_)
                | toml::Value::Integer(_)
                | toml::Value::Boolean(_)
                | toml::Value::Float(_) => {}
                _ => bail!("オプション {name} の値は文字列・数値・真偽値で指定してください"),
            }
        }
        let changed = options
            .iter()
            .filter(|(name, value)| current.get(*name) != Some(*value))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        Ok(Some(OptionsUpdate {
            options,
            changed,
            stamp,
        }))
    }

    pub fn accept(&mut self, update: OptionsUpdate) {
        // 送信前に時刻を確定すると、途中失敗した更新を次局で再試行できなくなる。
        self.stamp = update.stamp;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "csa_options_reload_{}_{}_{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed),
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::write(&path, "[engine.options]\nBookFile = 'old.db'\n").unwrap();
            Self(path)
        }

        fn write(&self, content: &str) {
            std::fs::write(&self.0, content).unwrap();
            let modified = SystemTime::now() + std::time::Duration::from_secs(1);
            std::fs::File::options()
                .write(true)
                .open(&self.0)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }

        fn current(&self) -> Options {
            toml::from_str::<ReloadConfig>(&std::fs::read_to_string(&self.0).unwrap())
                .unwrap()
                .engine
                .options
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }

    #[test]
    fn only_changed_options_are_sent_and_accepted_updates_are_not_repeated() {
        let fixture = Fixture::new();
        let mut reloader = OptionsReloader::new(&fixture.0).unwrap();
        let current = fixture.current();
        let cli = Cli::parse_from(["csa_client"]);
        assert!(reloader.prepare(&current, &cli).unwrap().is_none());
        fixture
            .write("[server]\nport = 'ignored'\n[engine.options]\nBookFile='new.db'\nThreads=4\n");
        let update = reloader.prepare(&current, &cli).unwrap().unwrap();
        assert_eq!(update.changed.len(), 2);
        assert_eq!(update.changed["BookFile"].as_str(), Some("new.db"));
        let new_options = update.options.clone();
        reloader.accept(update);
        assert!(reloader.prepare(&new_options, &cli).unwrap().is_none());
    }

    #[test]
    fn cli_options_keep_precedence_after_reload() {
        let fixture = Fixture::new();
        let reloader = OptionsReloader::new(&fixture.0).unwrap();
        let current = fixture.current();
        fixture.write("[engine.options]\nBookFile='new.db'\nUSI_Hash=1\nThreads=2\n");
        let cli = Cli::parse_from(["csa_client", "--hash", "512", "--options", "Threads=8"]);
        let update = reloader.prepare(&current, &cli).unwrap().unwrap();
        assert_eq!(update.options["USI_Hash"].as_integer(), Some(512));
        assert_eq!(update.options["Threads"].as_integer(), Some(8));
        assert_eq!(update.options["BookFile"].as_str(), Some("new.db"));
    }

    #[test]
    fn invalid_or_removed_options_leave_update_pending() {
        let fixture = Fixture::new();
        let reloader = OptionsReloader::new(&fixture.0).unwrap();
        let current = fixture.current();
        let cli = Cli::parse_from(["csa_client"]);
        for content in [
            "invalid TOML [",
            "[engine.options]\nThreads=8\n",
            "[engine.options]\nBookFile=['new.db']\n",
            "[engine.options]\nBookFile='''new\ncommand'''\n",
        ] {
            fixture.write(content);
            assert!(reloader.prepare(&current, &cli).is_err());
            assert!(reloader.prepare(&current, &cli).is_err());
            assert_eq!(current["BookFile"].as_str(), Some("old.db"));
        }
        fixture.write("[engine.options]\nBookFile='fixed.db'\n");
        assert!(reloader.prepare(&current, &cli).unwrap().is_some());
    }

    #[test]
    fn missing_file_does_not_acknowledge_the_update() {
        let fixture = Fixture::new();
        let reloader = OptionsReloader::new(&fixture.0).unwrap();
        let current = fixture.current();
        let cli = Cli::parse_from(["csa_client"]);
        std::fs::remove_file(&fixture.0).unwrap();
        assert!(reloader.prepare(&current, &cli).is_err());
        fixture.write("[engine.options]\nBookFile='restored.db'\n");
        assert!(reloader.prepare(&current, &cli).unwrap().is_some());
    }

    #[test]
    fn failed_send_can_retry_same_file_stamp() {
        let fixture = Fixture::new();
        let reloader = OptionsReloader::new(&fixture.0).unwrap();
        let current = fixture.current();
        fixture.write("[engine.options]\nBookFile='new.db'\n");
        let cli = Cli::parse_from(["csa_client"]);
        let first = reloader.prepare(&current, &cli).unwrap().unwrap();
        let second = reloader.prepare(&current, &cli).unwrap().unwrap();
        assert_eq!(first.changed, second.changed);
    }
}
