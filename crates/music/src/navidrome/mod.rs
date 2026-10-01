pub(crate) mod auth;
mod client;
mod lyrics;
mod playback;
mod wire;

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;

pub use client::NavidromeClient;
pub use lyrics::NavidromeLyrics;

use crate::navidrome::playback::Factory;
use crate::{Capabilities, MusicApi as _, MusicProvider, ProviderSession, Shape, SignIn};

pub struct NavidromeProvider;

impl NavidromeProvider {
    pub fn new() -> Self {
        Self
    }

    async fn connect(
        server: String,
        username: String,
        password: String,
    ) -> Result<ProviderSession> {
        let server = auth::normalize_server(&server)?;
        let client = NavidromeClient::new(
            server.clone(),
            username.clone(),
            password.clone(),
            auth::Session::default(),
        );
        client
            .sign_in()
            .await
            .context("cannot sign in to the navidrome server")?;
        let profile = client
            .profile()
            .await
            .context("cannot reach the navidrome server")?;
        auth::store(&auth::Credentials {
            server,
            username,
            password,
            session: client.session(),
        })?;
        Ok(ProviderSession {
            profile,
            api: Arc::new(client.clone()),
            playback: Arc::new(Factory::new(client)),
            shape: Shape::Catalog,
            authenticated: true,
            capabilities: Capabilities::ALL,
        })
    }

    async fn restore_stored() -> Result<Option<ProviderSession>> {
        let Some(mut remembered) = auth::load() else {
            return Ok(None);
        };
        let client = NavidromeClient::new(
            remembered.server.clone(),
            remembered.username.clone(),
            remembered.password.clone(),
            remembered.session.clone(),
        );
        match client.profile().await {
            Ok(profile) => {
                let session = client.session();
                if session != remembered.session {
                    remembered.session = session;
                    if let Err(error) = auth::store(&remembered) {
                        log::warn!("navidrome: cannot keep the renewed session: {error:#}");
                    }
                }
                Ok(Some(ProviderSession {
                    profile,
                    api: Arc::new(client.clone()),
                    playback: Arc::new(Factory::new(client)),
                    shape: Shape::Catalog,
                    authenticated: true,
                    capabilities: Capabilities::ALL,
                }))
            }
            Err(error) if crate::trouble::offline(&format!("{error:#}")) => Err(error),
            Err(error) => {
                log::warn!("navidrome: the stored session is no longer usable: {error:#}");
                Ok(None)
            }
        }
    }
}

impl Default for NavidromeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicProvider for NavidromeProvider {
    fn name(&self) -> &'static str {
        "Navidrome"
    }

    fn slug(&self) -> &'static str {
        "navidrome"
    }

    fn sign_in_options(&self) -> Vec<SignIn> {
        vec![SignIn::Credentials {
            server: String::new(),
            username: String::new(),
            password: String::new(),
        }]
    }

    fn stored(&self) -> bool {
        auth::load().is_some()
    }

    fn location(&self) -> Option<String> {
        auth::load().map(|credentials| credentials.server)
    }

    /// The configured server's host, since a self-hosted library has no address in common with
    /// anyone else's.
    fn reach(&self) -> Option<String> {
        let server = auth::load()?.server;
        let host = server
            .split_once("://")
            .map_or(server.as_str(), |(_, rest)| rest);
        let host = host.split(['/', ':', '?']).next()?;
        (!host.is_empty()).then(|| host.to_owned())
    }

    async fn restore(&self) -> Result<Option<ProviderSession>> {
        Self::restore_stored().await
    }

    async fn sign_in(
        &self,
        method: SignIn,
        _prompt: crate::PromptSink,
        _input: crate::InputSource,
    ) -> Result<ProviderSession> {
        let SignIn::Credentials {
            server,
            username,
            password,
        } = method
        else {
            anyhow::bail!("navidrome signs in with a server address, username and password")
        };
        if username.trim().is_empty() {
            anyhow::bail!("the navidrome username is empty");
        }
        if password.is_empty() {
            anyhow::bail!("the navidrome password is empty");
        }
        Self::connect(server, username, password).await
    }

    fn sign_out(&self) {
        auth::forget();
    }
}
