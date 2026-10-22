//! Native session signaling, independent of the rendezvous provider.
use anyhow::{Context, Result};
use serde_json::Value;
use std::net::IpAddr;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) const PREPARATION_WAIT: std::time::Duration = std::time::Duration::from_secs(125);

#[async_trait::async_trait]
pub trait SessionSignaling: Send {
    /// Implementations must authenticate the remote session before returning SDP.
    async fn send(&mut self, message: Value) -> Result<()>;
    async fn receive(&mut self) -> Result<Value>;
    /// An authenticated host may wait for local compositor consent before
    /// returning its first answer. Other protocol reads keep their own limits.
    async fn receive_answer(&mut self) -> Result<Value> {
        self.receive().await
    }
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
    async fn receive_answer(&mut self) -> Result<Value> {
        crate::native::read_message_wait(&mut self.stream, PREPARATION_WAIT).await
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn system_consent_wait_extends_only_the_authenticated_answer_phase() {
        let (mut normal_writer, normal_reader) = tokio::io::duplex(1024);
        let (mut answer_writer, answer_reader) = tokio::io::duplex(1024);
        let peer = "127.0.0.1".parse().unwrap();
        let mut normal = TlsSignaling {
            stream: normal_reader,
            peer,
        };
        let mut answer = TlsSignaling {
            stream: answer_reader,
            peer,
        };
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(16)).await;
            let value = serde_json::json!({"generated":"authorized-answer"});
            crate::native::write_message(&mut normal_writer, &value)
                .await
                .unwrap();
            crate::native::write_message(&mut answer_writer, &value)
                .await
                .unwrap();
        });
        let (normal, answer) = tokio::join!(normal.receive(), answer.receive_answer());
        assert!(normal.unwrap_err().to_string().contains("timed out"));
        assert_eq!(answer.unwrap()["generated"], "authorized-answer");
        writer.await.unwrap();
    }
}
