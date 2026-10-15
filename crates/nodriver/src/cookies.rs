//! Browser-wide cookies. Mirrors nodriver's `CookieJar` in `core/browser.py`.

use std::path::Path;

use nodriver_cdp::{network, storage};

use crate::connection::Connection;
use crate::error::Result;

/// Cookies of the default browser context, through the `Storage` domain on the browser endpoint.
/// Get it from [`crate::Browser::cookies`].
pub struct CookieJar<'a> {
    conn: &'a Connection,
}

impl<'a> CookieJar<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Every cookie of every site.
    pub async fn get_all(&self) -> Result<Vec<network::Cookie>> {
        Ok(self
            .conn
            .send(&storage::GetCookiesParams::new(), None)
            .await?
            .cookies)
    }

    pub async fn set_all(&self, cookies: Vec<network::CookieParam>) -> Result<()> {
        self.conn
            .send(&storage::SetCookiesParams::new(cookies), None)
            .await?;
        Ok(())
    }

    /// Deletes every cookie.
    pub async fn clear(&self) -> Result<()> {
        self.conn.send(&storage::ClearCookiesParams::new(), None).await?;
        Ok(())
    }

    /// Writes every cookie to `path` as JSON (a list of `Network.Cookie`). Returns how many.
    pub async fn save(&self, path: impl AsRef<Path>) -> Result<usize> {
        self.save_filtered(path, |_| true).await
    }

    /// Writes the cookies accepted by `keep` to `path`. nodriver takes a regex matched against
    /// domain, name and value; here the filter is a closure.
    pub async fn save_filtered<F>(&self, path: impl AsRef<Path>, keep: F) -> Result<usize>
    where
        F: Fn(&network::Cookie) -> bool,
    {
        let cookies: Vec<network::Cookie> = self.get_all().await?.into_iter().filter(|c| keep(c)).collect();
        crate::tab::write_file(path.as_ref(), &serde_json::to_vec_pretty(&cookies)?)?;
        Ok(cookies.len())
    }

    /// Restores cookies written by [`CookieJar::save`]. Returns how many were set.
    pub async fn load(&self, path: impl AsRef<Path>) -> Result<usize> {
        self.load_filtered(path, |_| true).await
    }

    /// Restores the cookies from `path` accepted by `keep`.
    pub async fn load_filtered<F>(&self, path: impl AsRef<Path>, keep: F) -> Result<usize>
    where
        F: Fn(&network::Cookie) -> bool,
    {
        let saved: Vec<network::Cookie> = serde_json::from_slice(&std::fs::read(path.as_ref())?)?;
        let params: Vec<network::CookieParam> = saved.iter().filter(|c| keep(c)).map(to_param).collect();
        let n = params.len();
        if n > 0 {
            self.set_all(params).await?;
        }
        Ok(n)
    }
}

/// The fields of a read cookie that `Storage.setCookies` accepts back.
pub fn to_param(c: &network::Cookie) -> network::CookieParam {
    network::CookieParam {
        name: c.name.clone(),
        value: c.value.clone(),
        url: None,
        domain: Some(c.domain.clone()),
        path: Some(c.path.clone()),
        secure: Some(c.secure),
        http_only: Some(c.http_only),
        same_site: c.same_site.clone(),
        expires: c.expires.filter(|e| !c.session && *e > 0.0),
        priority: Some(c.priority.clone()),
        source_scheme: Some(c.source_scheme.clone()),
        source_port: Some(c.source_port),
        partition_key: c.partition_key.clone(),
    }
}
