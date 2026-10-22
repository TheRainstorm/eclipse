//! Native session signaling, independent of the rendezvous provider.
use anyhow::{Context, Result};
use serde_json::Value;
use std::net::IpAddr;
use tokio::io::{AsyncRead, AsyncWrite};

#[async_trait::async_trait]
pub trait SessionSignaling: Send {
    /// Implementations must authenticate the remote session before returning SDP.
    async fn send(&mut self, message: Value) -> Result<()>;
    async fn receive(&mut self) -> Result<Value>;
    /// LAN's authenticated TCP address pins its host candidates. Internet
    /// providers use authenticated SDP and allow direct UDP candidates instead.
    fn lan_peer(&self) -> Option<IpAddr> {
        None
    }
    async fn disconnected(&mut self) -> Result<()> {
        let _ = self.receive().await?;
        anyhow::bail!("unexpected renegotiation in native session")
    }
}

pub(crate) struct TlsSignaling<S> {
    pub stream: S,
    pub peer: IpAddr,
}

#[async_trait::async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> SessionSignaling for TlsSignaling<S> {
    async fn send(&mut self, message: Value) -> Result<()> {
        crate::native::write_message(&mut self.stream, &message).await
    }
    async fn receive(&mut self) -> Result<Value> {
        crate::native::read_message(&mut self.stream).await
    }
    fn lan_peer(&self) -> Option<IpAddr> {
        Some(self.peer)
    }
    async fn disconnected(&mut self) -> Result<()> {
        use tokio::io::AsyncReadExt;
        let _ = self
            .stream
            .read_u8()
            .await
            .context("native signaling closed");
        Ok(())
    }
}
