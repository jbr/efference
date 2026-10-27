//! A server's stored credentials as a mounted HTTP transport sends them, and their
//! renewal while the session runs (docs/mcp-auth.md).
//!
//! The grant an authorization produced lives in the store (`store.rs`); a mount
//! takes a [`Credentials`] over it and the transport sends its header on every
//! request. When the grant passes its expiry, or the server refuses it with a
//! `401`, the transport renews it through [`Credentials::renew`], the same
//! refresh the mount runs on a stale grant. A renewal holds the store's lock for
//! the server, re-reads the stored grant, and takes that grant without
//! refreshing when another renewal already replaced the one these credentials
//! hold: that is what makes concurrent calls across a host's sessions share one
//! refresh, and keeps a rotated refresh token from being presented twice.

use std::sync::Arc;

use anyhow::Context as _;

use super::store::TokenStore;
use crate::oauth::token::{self, Tokens};

/// One server's grant, held by the transport that sends it, with the store it
/// was read from and is renewed into.
pub struct Credentials {
    server: String,
    store: Arc<dyn TokenStore>,
    tokens: Tokens,
}

impl Credentials {
    /// The credentials `store` holds for `server` as they stand, expired or not;
    /// `None` when nothing is stored.
    pub fn load(store: &Arc<dyn TokenStore>, server: &str) -> anyhow::Result<Option<Self>> {
        Ok(store
            .load(server)?
            .map(|stored| Self::new(store, server, stored.tokens)))
    }

    /// Credentials over a grant just written to `store` for `server`.
    pub(super) fn new(store: &Arc<dyn TokenStore>, server: &str, tokens: Tokens) -> Self {
        Self {
            server: server.to_string(),
            store: Arc::clone(store),
            tokens,
        }
    }

    /// The `Authorization` header value to send.
    pub fn header(&self) -> String {
        self.tokens.authorization_header()
    }

    /// Whether the grant is at or past its stated expiry. A grant that states
    /// none never is; the server's `401` is the only signal for it.
    pub fn is_expired(&self) -> bool {
        self.tokens.is_expired()
    }

    /// Replace the grant these credentials hold: with the stored one, when a
    /// renewal elsewhere already moved it past what is held here and it has not
    /// expired, and otherwise by refreshing the stored grant and saving the
    /// result. `Err` carries why it could not be renewed, in words for the
    /// call's result: nothing stored, no refresh token, the refresh refused, the
    /// issuer unreachable.
    ///
    /// The stored grant is the one refreshed, not the one held here: under
    /// rotation it carries the only refresh token the issuer still honours.
    pub async fn renew(&mut self) -> Result<(), String> {
        let lock = self.store.refresh_lock(&self.server);
        let _refreshing = lock.lock().await;
        self.renew_locked().await.map_err(|e| format!("{e:#}"))
    }

    async fn renew_locked(&mut self) -> anyhow::Result<()> {
        let mut stored = self
            .store
            .load(&self.server)?
            .context("no sign-in is stored for it")?;
        if stored.tokens.access_token() != self.tokens.access_token() && !stored.tokens.is_expired()
        {
            log::debug!(
                "taking the grant another renewal stored for `{}`",
                self.server
            );
            self.tokens = stored.tokens;
            return Ok(());
        }
        log::debug!("refreshing the grant for `{}`", self.server);
        let tokens = token::refresh(
            &crate::oauth::http_client(),
            &stored.token_endpoint,
            &stored.tokens,
            &stored.client_id,
            &stored.resource,
        )
        .await?;
        stored.tokens = tokens.clone();
        // The new grant is sent either way: the issuer has already rotated the
        // old refresh token away, so failing the call would not bring it back.
        if let Err(e) = self.store.save(&self.server, &stored) {
            log::warn!(
                "could not store the refreshed grant for `{}`; the next session signs in again: {e:#}",
                self.server
            );
        }
        self.tokens = tokens;
        Ok(())
    }
}

/// The credentials to mount `server` with, from what `store` holds.
///
/// A grant past its expiry is renewed here rather than being sent and left to
/// fail — one round trip beats a `401` plus a full rediscovery. A renewal that
/// fails is *not* an error: the mount goes ahead without credentials and the
/// server's `401`, with its challenge, starts the dance, so a stale grant is
/// never worse than no grant at all.
pub async fn stored_credentials(
    store: &Arc<dyn TokenStore>,
    server: &str,
) -> anyhow::Result<Option<Credentials>> {
    let Some(mut credentials) = Credentials::load(store, server)? else {
        return Ok(None);
    };
    if credentials.is_expired()
        && let Err(why) = credentials.renew().await
    {
        log::debug!("renewing the grant for `{server}` failed, falling back to authorizing: {why}");
        return Ok(None);
    }
    Ok(Some(credentials))
}
