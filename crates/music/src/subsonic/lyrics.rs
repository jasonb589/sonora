use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use opensubsonic::Auth;
use opensubsonic::data::StructuredLyrics;

use crate::lyrics::lrc;
use crate::subsonic::{SubsonicProvider, auth};
use crate::{Lyrics, LyricsHit, LyricsLine, LyricsProvider, LyricsQuery, MusicProvider};

/// A sheet a subsonic server hands over comes out of the files it serves, so it is the file's own
/// the way a local file's tags are, and it is trusted as much.
const SOURCE: &str = "Subsonic";
const TRUST: u32 = 100;
const CLIENT_NAME: &str = "sonora";

/// Lyrics for the tracks a subsonic server holds, which navidrome answers from each file's tags
/// or from the `.lrc` file beside it.
pub struct SubsonicLyrics;

impl SubsonicLyrics {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SubsonicLyrics {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for SubsonicLyrics {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        // Only a track this server holds carries the id the endpoint reads lyrics by.
        let slug = MusicProvider::slug(&SubsonicProvider::new());
        let Some(key) = query.track.as_ref().filter(|key| key.provider == slug) else {
            return Ok(Vec::new());
        };
        let Some(credentials) = auth::load() else {
            return Ok(Vec::new());
        };
        let client = opensubsonic::Client::new(
            &credentials.server,
            Auth::token(&credentials.username, &credentials.password),
        )
        .context("cannot parse the subsonic server address")?
        .with_client_name(CLIENT_NAME);
        let found = client
            .get_lyrics_by_song_id(&key.id, Some(true))
            .await
            .context("cannot reach the subsonic lyrics")?;
        let Some(lyrics) = sheet(&found.structured_lyrics) else {
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

/// The sheet to show of those the server listed: a synced one that reads as the lyrics rather
/// than as a translation of them when there is any, since the player can follow it, else the
/// first of them, untimed.
fn sheet(entries: &[StructuredLyrics]) -> Option<Lyrics> {
    let main: Vec<&StructuredLyrics> = entries
        .iter()
        .filter(|entry| matches!(entry.kind.as_deref(), None | Some("main")))
        .filter(|entry| !entry.line.is_empty())
        .collect();
    let entry = main
        .iter()
        .copied()
        .find(|entry| entry.synced)
        .or_else(|| main.first().copied())?;
    let lines = timed(entry);
    match lines.is_empty() {
        true => Some(Lyrics::plain(untimed(entry))),
        false => Some(Lyrics::Synced {
            lines: lines.into(),
        }),
    }
}

/// The lines of a sheet, read the way a file's own lyrics are, from the sheet written back out as
/// LRC: a server leaves the `[00:12.345]` marks of a sheet that stamps each of its characters in
/// the lines it hands over, and the reader times them and keeps them out of the text.
fn timed(entry: &StructuredLyrics) -> Vec<LyricsLine> {
    let offset = millis(entry.offset.unwrap_or(0.));
    let text = entry
        .line
        .iter()
        .map(|line| {
            let at = offset + millis(line.start.unwrap_or(0.));
            format!("[{}]{}", stamp(at), line.value.trim())
        })
        .collect::<Vec<_>>()
        .join("\n");
    lrc::parse(&text)
}

/// A time the way LRC writes one.
fn stamp(at: Duration) -> String {
    let millis = at.as_millis();
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1_000 % 60,
        millis % 1_000
    )
}

fn untimed(entry: &StructuredLyrics) -> String {
    entry
        .line
        .iter()
        .map(|line| line.value.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

fn millis(value: f64) -> Duration {
    Duration::from_millis(value.max(0.) as u64)
}

#[cfg(test)]
mod tests {
    use opensubsonic::data::Line;

    use super::*;

    fn given(lines: Vec<(&str, f64)>) -> Vec<LyricsLine> {
        let entry = StructuredLyrics {
            lang: "xxx".to_owned(),
            synced: true,
            line: lines
                .into_iter()
                .map(|(value, start)| Line {
                    value: value.to_owned(),
                    start: Some(start),
                })
                .collect(),
            display_artist: None,
            display_title: None,
            offset: None,
            kind: Some("main".to_owned()),
            agents: None,
            cue_line: None,
        };
        timed(&entry)
    }

    #[test]
    fn a_sheet_that_stamps_each_character_gives_its_marks_up_for_timed_words() {
        let lines = given(vec![
            ("a[00:00.100]b[00:00.200]c", 0.),
            ("d[00:00.500]e", 400.),
        ]);

        assert_eq!(lines[0].text, "abc");
        assert_eq!(lines[0].start, Duration::ZERO);
        assert_eq!(lines[1].text, "de");
        assert_eq!(lines[1].start, Duration::from_millis(400));
        let words = lines[0].words.as_ref().expect("the line is worded");
        assert_eq!(words.len(), 3);
        assert_eq!(words[1].text, "b");
        assert_eq!(words[1].start, Duration::from_millis(100));
    }

    #[test]
    fn a_plain_sheet_keeps_the_times_the_server_gave_it() {
        let lines = given(vec![("first line", 0.), ("second line", 12_500.)]);

        assert_eq!(lines[0].text, "first line");
        assert_eq!(lines[0].start, Duration::ZERO);
        assert_eq!(lines[0].end, Some(Duration::from_millis(12_500)));
        assert_eq!(lines[1].start, Duration::from_millis(12_500));
        assert!(lines[1].words.is_none());
    }
}
