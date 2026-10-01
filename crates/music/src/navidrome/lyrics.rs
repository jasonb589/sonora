use anyhow::{Context as _, Result};
use async_trait::async_trait;

use crate::navidrome::{NavidromeProvider, auth, client::NavidromeClient};
use crate::{LyricsHit, LyricsProvider, LyricsQuery, MusicProvider};

/// A sheet a navidrome server hands over comes out of the files it serves, so it is the file's
/// own the way a local file's tags are, and it is trusted as much.
const SOURCE: &str = "Navidrome";
const TRUST: u32 = 100;

/// Lyrics for the tracks a navidrome server holds, which the server keeps as the file's own tag
/// text and hands over a line at a time.
pub struct NavidromeLyrics;

impl NavidromeLyrics {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NavidromeLyrics {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for NavidromeLyrics {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        // only a track this server holds carries the id the endpoint reads lyrics by
        let slug = MusicProvider::slug(&NavidromeProvider::new());
        let Some(key) = query.track.as_ref().filter(|key| key.provider == slug) else {
            return Ok(Vec::new());
        };
        let Some(stored) = auth::load() else {
            return Ok(Vec::new());
        };
        let client = NavidromeClient::new(
            stored.server,
            stored.username,
            stored.password,
            stored.session,
        );
        let Some(lyrics) = client
            .lyrics(&key.id)
            .await
            .context("cannot reach the navidrome lyrics")?
        else {
            return Ok(Vec::new());
        };

        Ok(vec![LyricsHit {
            source: SOURCE,
            trust: TRUST,
            lyrics,
            instrumental: false,
            title: query.title.clone(),
            artist: query.artist.clone(),
            album: query.album.clone(),
            duration: Some(query.duration),
            writers: Vec::new(),
        }])
    }
}
