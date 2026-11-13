//! Optional wallpaper discovery. Downloading, decoding and display stay in the host.
use crate::anyhow::Result;
use std::{future::Future, pin::Pin};

pub use reqwest::Client;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub id: String,
    pub name: String,
    /// Changes when this source's saved filters change; never contains secrets.
    pub revision: String,
}

pub type Resolution<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

pub trait Provider: Send + Sync {
    fn sources(&self) -> Vec<Source>;
    /// Return a direct image URL. The host owns the deadline and cancellation.
    fn resolve<'a>(&'a self, client: Client, source: &'a str) -> Resolution<'a>;
}
