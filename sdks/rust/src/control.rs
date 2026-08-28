//! Browser control via CDP — gated behind the default `control` feature.
//!
//! Connects [`chromiumoxide`](https://docs.rs/chromiumoxide) to the engine's
//! remote-debugging endpoint, mirroring the patchright attachment in the
//! Python/Node SDKs. [`ShardX::session`](crate::ShardX::session) returns a
//! [`Session`] that owns both the driven browser and the engine process.

use anyhow::{anyhow, Result};
use chromiumoxide::{Browser as CdpBrowser, Page};
use futures_util::StreamExt;
use tokio::task::JoinHandle;

use crate::browser::BrowserSession;

/// A launched ShardX engine with a connected CDP client.
///
/// Drive it through [`Session::browser`] / [`Session::new_page`], then call
/// [`Session::close`] to disconnect and stop the engine.
pub struct Session {
    /// The connected chromiumoxide browser.
    pub browser: CdpBrowser,
    /// The underlying engine process + launch decisions.
    pub engine: BrowserSession,
    handler: JoinHandle<()>,
}

impl Session {
    pub(crate) async fn connect(engine: BrowserSession) -> Result<Self> {
        let ws = engine
            .cdp_url
            .clone()
            .ok_or_else(|| anyhow!("CDP endpoint unavailable — launch with cdp = true"))?;
        let (browser, mut handler) = CdpBrowser::connect(ws).await?;
        // The handler future must be polled for CDP traffic to flow.
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok(Self {
            browser,
            engine,
            handler,
        })
    }

    /// The browser-level CDP websocket URL.
    pub fn cdp_url(&self) -> Option<&str> {
        self.engine.cdp_url.as_deref()
    }

    /// Open a new tab navigated to `url`.
    pub async fn new_page(&self, url: &str) -> Result<Page> {
        Ok(self.browser.new_page(url).await?)
    }

    /// Disconnect the CDP client and stop the engine process. Idempotent-ish:
    /// consumes `self`.
    pub async fn close(mut self) -> Result<()> {
        let _ = self.browser.close().await;
        self.handler.abort();
        self.engine.stop().await?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.handler.abort();
    }
}
