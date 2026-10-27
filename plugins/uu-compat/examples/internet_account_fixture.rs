//! Local-only normal device registration for isolated account testing.
use anyhow::{Context,Result};
#[tokio::main]
async fn main()->Result<()> {
 let source=uu_compat::Config::from_env()?;
 let state=uu_compat::ClientState::load(&source.at("state.json"))?;
 let root=std::env::args().nth(1).context("destination directory required")?;
 let config=uu_compat::Config{data_dir:std::path::PathBuf::from(&root),state_dir:std::path::PathBuf::from(&root).join("state"),signaling_host:source.signaling_host};
 config.ensure_dirs().await?;
 let mut api=uu_compat::UuApi::new(uu_compat::SignalClient::new(&config)?,config.at("state.json"))?;
 if !api.has_identity(){api.register("Eclipse internet generated test").await?;}
 api.store_token(state.token.as_deref().context("existing login missing")?,state.user_id)?;
 let response=api.device_list().await?;
 println!("normal_authenticated_device_list_code={}",response["code"].as_i64().unwrap_or(-1));
 Ok(())
}
