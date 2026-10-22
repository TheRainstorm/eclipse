//! Reader for immutable legacy connection snapshots and migration.
use std::path::Path;
use uu_core::internet::Settings;

pub(super) fn read_profile(path: &Path) -> anyhow::Result<Settings> {
    anyhow::ensure!(std::fs::metadata(path)?.len() <= 64 * 1024, "连接配置文件过大");
    serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("连接配置文件格式错误，原文件已保留"))
}
