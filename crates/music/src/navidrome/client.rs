use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::task::JoinSet;

use crate::engine::Loudness;
use crate::escape;
use crate::navidrome::auth;
use crate::navidrome::wire;
use crate::{
    Album, AlbumCatalogue, AlbumDetail, Artist, ArtistProfile, Genre, GenreDetail, GenreItem,
    GenreSection, HomeFeed, Lyrics, MediaKind, MusicApi, Page, Pages, Playlist, PlaylistDetail,
    Report, SUGGESTIONS, SavedArtist, Track, UserProfile, distinct_covers,
};

/// How many artists a page asks for pictures of at once.
const PORTRAIT_LIMIT: usize = 24;
/// How many rows a library page asks the server for at a time. Every listing call carries a
/// window, since a call without one is answered with the whole table.
const LIBRARY_PAGE: usize = 500;
/// How many pages may sit behind the one being drawn.
const PAGE_DEPTH: usize = 2;
const RADIO_COUNT: usize = 25;
const GENRE_ALBUMS: usize = 50;
const HOME_SONGS: usize = 25;
const HOME_ALBUMS: usize = 12;
const TOP_TRACKS: usize = 20;
const SEARCH_SONGS: usize = 50;
const SEARCH_ALBUMS: usize = 30;
const RECENT_COUNT: usize = 50;
const API_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "sonora";
/// The header the native api takes its bearer token in, and hands a stretched one back in.
const AUTHORIZATION: &str = "X-Nd-Authorization";
/// How many times a read the connection dropped is asked for again. A host that closes a
/// connection under one request usually takes the next one, and album art or a listing that
/// never arrives costs the screen far more than the second request does.
const ASK_ATTEMPTS: usize = 3;

/// Where the server says how long a listing is, which never rides in the body.
const TOTAL_COUNT: &str = "x-total-count";

#[derive(Clone)]
pub struct NavidromeClient {
    inner: Arc<Inner>,
}

struct Inner {
    server: String,
    username: String,
    password: String,
    http: reqwest::Client,
    /// What sign-in handed back, behind a lock rather than copied: a clone that signs in again
    /// signs every other clone in with it.
    session: RwLock<auth::Session>,
}

/// What the server records about a track that playback wants before the decoder can tell.
#[derive(Clone, Copy, Debug, Default)]
pub struct Details {
    pub duration: Option<Duration>,
    /// The track's ReplayGain, which navidrome reads out of the file's own tags.
    pub loudness: Option<Loudness>,
}

impl NavidromeClient {
    pub fn new(server: String, username: String, password: String, session: auth::Session) -> Self {
        Self {
            inner: Arc::new(Inner {
                server: server.trim_end_matches('/').to_owned(),
                username,
                password,
                http: reqwest::Client::new(),
                session: RwLock::new(session),
            }),
        }
    }

    /// What sign-in has left so far, for the caller that stores it.
    pub fn session(&self) -> auth::Session {
        let Ok(session) = self.inner.session.read() else {
            return auth::Session::default();
        };
        session.clone()
    }

    /// Signs in, keeping the bearer token the native api takes and the subsonic token and salt
    /// every `rest` url is signed with. A signature already held stays: it never expires, so the
    /// cover urls built from it keep working and every image cache keeps its picture.
    pub async fn sign_in(&self) -> Result<()> {
        let url = format!("{}/auth/login", self.inner.server);
        let asked = serde_json::json!({
            "username": self.inner.username.as_str(),
            "password": self.inner.password.as_str(),
        });
        let answer = self
            .inner
            .http
            .post(url)
            .json(&asked)
            .send()
            .await
            .context("cannot reach the navidrome server")?
            .error_for_status()
            .context("navidrome refused the sign-in")?
            .text()
            .await
            .context("cannot read the navidrome answer")?;
        let mut session: auth::Session = serde_json::from_str(&answer)
            .context("the navidrome sign-in answered with no session")?;
        if session.jwt.is_empty() {
            anyhow::bail!("navidrome signed in without a bearer token");
        }
        let held = self.session();
        if !held.subsonic_token.is_empty() {
            session.subsonic_token = held.subsonic_token;
            session.subsonic_salt = held.subsonic_salt;
        }
        if let Ok(mut sign_in) = self.inner.session.write() {
            *sign_in = session;
        }
        Ok(())
    }

    /// A `rest` route with the account and the signature already on it. Those routes take no
    /// bearer token, the query signs them, which is how navidrome's own ui streams a track and
    /// fetches a cover. `None` before sign-in, when there is nothing to sign with.
    fn rest(&self, route: &str) -> Option<String> {
        let session = self.session();
        if session.subsonic_token.is_empty() || session.subsonic_salt.is_empty() {
            return None;
        }
        let server = &self.inner.server;
        let user = escape::component(&self.inner.username);
        let token = escape::component(&session.subsonic_token);
        let salt = escape::component(&session.subsonic_salt);
        let query = format!("u={user}&t={token}&s={salt}&v={API_VERSION}");
        Some(format!(
            "{server}/rest/{route}?{query}&c={CLIENT_NAME}&f=json"
        ))
    }

    /// A `rest` call the app builds a url for, which a session without a signature cannot make.
    fn rest_of(&self, route: &str) -> Result<String> {
        match self.rest(route) {
            Some(url) => Ok(url),
            None => anyhow::bail!("the navidrome session carries no signature"),
        }
    }

    /// The cover the server holds for an artwork id, at the size it was stored in: the original
    /// is the only size that is never blurry, and the ui scales whatever arrives down to the
    /// pixel edge it paints.
    fn cover(&self, id: &str) -> Option<String> {
        let base = self.rest("getCoverArt")?;
        Some(format!("{base}&id={}&size=0", escape::component(id)))
    }

    /// The cover of a song: its own when the file carries art, since a single may have its own,
    /// else the cover of the album it is on, which is the fallback the server itself makes.
    fn song_cover(&self, source: &wire::Song) -> Option<String> {
        let art = match source.has_cover_art || source.album_id.is_empty() {
            true => format!("mf-{}", source.id),
            false => format!("al-{}", source.album_id),
        };
        self.cover(&art)
    }

    /// The picture of an album: the artwork the server holds when the files on it carry any, else
    /// the url an outside service gave it, which is the order the server's own ui draws them in.
    fn album_cover(&self, source: &wire::Album) -> Option<String> {
        if !source.image_hash.is_empty() {
            return self.cover(&format!("al-{}", source.id));
        }
        let url = source.large_image_url.trim();
        match url.is_empty() {
            true => None,
            false => Some(url.to_owned()),
        }
    }

    /// The picture of an artist: the artwork the server holds when it has any, else the url an
    /// outside service gave it, which is the order the server's own ui draws them in.
    fn artist_cover(&self, source: &wire::Artist, id: &str) -> Option<String> {
        if !source.image_absent {
            return self.cover(&format!("ar-{id}"));
        }
        let url = source.large_image_url.trim();
        match url.is_empty() {
            true => None,
            false => Some(url.to_owned()),
        }
    }

    /// The cover of a playlist, which the server composes out of its tracks: one with no tracks
    /// has nothing to compose and would answer with an error rather than a picture.
    fn playlist_cover(&self, source: &wire::Playlist) -> Option<String> {
        match source.song_count {
            0 => None,
            _ => self.cover(&format!("pl-{}", source.id)),
        }
    }

    fn song(&self, source: wire::Song) -> Track {
        let cover = self.song_cover(&source);
        wire::track(source, cover)
    }

    fn album_of(&self, source: wire::Album) -> Album {
        let cover = self.album_cover(&source);
        wire::album(source, cover.clone(), cover)
    }

    fn artist_of(&self, source: wire::Artist) -> SavedArtist {
        let cover = self.artist_cover(&source, &source.id);
        wire::saved_artist(&source, cover)
    }

    fn playlist_of(&self, source: wire::Playlist) -> Playlist {
        let cover = self.playlist_cover(&source);
        wire::playlist(&source, cover, &self.inner.username)
    }

    /// A row of a playlist, whose own id is the place the row holds in it rather than a song:
    /// the song the app plays is the file the row names.
    fn entry(&self, source: wire::PlaylistEntry) -> Track {
        let mut song = source.song;
        song.id = source.media_file_id;
        self.song(song)
    }

    /// One native call, with the bearer token the session holds. A token the server no longer
    /// takes is renewed once and the call repeated, since a web session lasts two days and the
    /// credentials that opened it outlive it.
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let answer = self.send(method.clone(), path, body).await?;
        if answer.status() != reqwest::StatusCode::UNAUTHORIZED {
            return answer
                .error_for_status()
                .with_context(|| format!("the navidrome server refused {path}"));
        }
        log::info!("navidrome: the session expired, signing in again");
        self.sign_in().await?;
        self.send(method, path, body)
            .await?
            .error_for_status()
            .with_context(|| format!("the navidrome server refused {path}"))
    }

    /// A read that the connection dropped is asked for again: a host that closes a connection
    /// under one request usually takes the next one, and a cover or a listing that never arrives
    /// costs the screen far more than the second request does. A call that changes something is
    /// never repeated, since one that failed may well have landed.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.once(&method, path, body).await {
                Ok(answer) => return Ok(answer),
                Err(error) if method == reqwest::Method::GET && attempt < ASK_ATTEMPTS => {
                    log::debug!("navidrome: {path} was dropped ({error:#}); asking again");
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// One call over the wire, with the token in hand and the one the answer carries kept for
    /// the next call.
    async fn once(
        &self,
        method: &reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let jwt = self.session().jwt;
        let mut asked = self
            .inner
            .http
            .request(method.clone(), format!("{}{path}", self.inner.server))
            .header(AUTHORIZATION, format!("Bearer {jwt}"));
        if let Some(body) = body {
            asked = asked.json(body);
        }
        let answer = asked
            .send()
            .await
            .context("cannot reach the navidrome server")?;
        // every answer hands the session back stretched by another two days
        if let Some(token) = answer.headers().get(AUTHORIZATION)
            && let Ok(token) = token.to_str()
        {
            self.keep(token);
        }
        Ok(answer)
    }

    /// Keeps a token the server stretched, so the session outlives the two days the one that
    /// went out had left.
    fn keep(&self, token: &str) {
        let Ok(mut held) = self.inner.session.write() else {
            return;
        };
        held.jwt = token.trim().to_owned();
    }

    /// A native read.
    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        self.request(reqwest::Method::GET, path, None).await
    }

    /// A native call that changes something, whose answer holds nothing the app keeps.
    async fn change(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<()> {
        let answer = self.request(method, path, body).await?;
        drop(answer);
        Ok(())
    }

    /// The body of an answer, as the shape the caller asked for.
    async fn read<T: DeserializeOwned>(&self, answer: reqwest::Response) -> Result<T> {
        let body = answer
            .text()
            .await
            .context("cannot read the navidrome answer")?;
        serde_json::from_str(&body).context("cannot read the navidrome answer")
    }

    /// One item of the native api, as `/api/album/{id}` answers it.
    async fn item<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let answer = self.get(path).await?;
        self.read(answer).await
    }

    /// One page of a listing, with how long the whole listing is, which the server keeps in an
    /// answer header.
    async fn listing<T: DeserializeOwned>(&self, path: &str) -> Result<(Vec<T>, Option<usize>)> {
        let answer = self.get(path).await?;
        let total = total_of(&answer);
        let rows = self.read(answer).await?;
        Ok((rows, total))
    }

    /// A listing that arrives a page at a time, so a library page draws its first rows while
    /// the rest are still coming. `query` names the listing and its order, and the window the
    /// server needs rides on the end of it. `keep` turns one row into what the app holds.
    fn paged<W, T>(
        &self,
        size: usize,
        query: &str,
        keep: impl Fn(&Self, W) -> T + Send + 'static,
    ) -> Pages<T>
    where
        W: DeserializeOwned + Send + 'static,
        T: Send + 'static,
    {
        let (sink, pages) = tokio::sync::mpsc::channel(PAGE_DEPTH);
        let client = self.clone();
        let query = query.to_owned();
        tokio::spawn(async move {
            let mut start = 0;
            loop {
                let asked = format!("{query}&_start={start}&_end={}", start + size);
                let (rows, total) = match client.listing::<W>(&asked).await {
                    Ok(page) => page,
                    Err(error) => {
                        sink.send(Err(error)).await.ok();
                        return;
                    }
                };
                let got = rows.len();
                let items = rows.into_iter().map(|row| keep(&client, row)).collect();
                if sink.send(Ok(Page { total, items })).await.is_err() {
                    return;
                }
                start += got;
                if got < size || total.is_some_and(|total| start >= total) {
                    return;
                }
            }
        });
        pages
    }

    /// One item of the native api that the app reads for what it records about a track.
    async fn song_source(&self, track_id: &str) -> Result<wire::Song> {
        let path = format!("/api/song/{}", escape::component(track_id));
        self.item(&path)
            .await
            .with_context(|| format!("cannot load the song {track_id}"))
    }

    /// The artist itself: the name the page shows, the biography and the picture.
    async fn artist_detail(&self, artist_id: &str) -> Result<wire::Artist> {
        let path = format!("/api/artist/{}", escape::component(artist_id));
        self.item(&path)
            .await
            .with_context(|| format!("cannot load the artist {artist_id}"))
    }

    /// The artist's albums, newest first. A failure only shortens the page, so it answers with
    /// whatever came back.
    async fn artist_albums(&self, artist_id: &str) -> Vec<Album> {
        let artist = escape::component(artist_id);
        let query = format!("/api/album?artist_id={artist}&_sort=maxYear&_order=DESC");
        let pages = self.paged(LIBRARY_PAGE, &query, Self::album_of);
        every(pages).await.unwrap_or_default()
    }

    /// The artist's most played songs. A failure only shortens the page, as for the albums.
    async fn top_tracks(&self, artist_id: &str) -> Vec<Track> {
        let artist = escape::component(artist_id);
        let query = format!("/api/song?artist_id={artist}&_sort=playCount&_order=DESC");
        let pages = self.paged(LIBRARY_PAGE, &query, Self::song);
        let rows = every(pages).await.unwrap_or_default();
        rows.into_iter().take(TOP_TRACKS).collect()
    }

    /// Up to `size` songs the server shuffles out of the whole library, for a shelf that wants
    /// something to play rather than something in particular.
    async fn random_songs(&self, size: usize) -> Vec<Track> {
        let path = format!("/api/song?_sort=random&_start=0&_end={size}");
        let rows: Vec<wire::Song> = self.item(&path).await.unwrap_or_default();
        rows.into_iter().map(|row| self.song(row)).collect()
    }

    /// Up to `size` albums in the order the server sorts them by, for a home shelf.
    async fn albums_sorted(&self, sort: &str, order: &str, size: usize) -> Vec<Album> {
        let path = format!("/api/album?_sort={sort}&_order={order}&_start=0&_end={size}");
        let rows: Vec<wire::Album> = self.item(&path).await.unwrap_or_default();
        rows.into_iter().map(|row| self.album_of(row)).collect()
    }

    /// Changes one field of a playlist. A `PUT` takes the whole record: a body naming only the
    /// changed field would blank every field it leaves out, so the record the server already
    /// holds goes back with that one field set.
    async fn change_playlist(
        &self,
        playlist_id: &str,
        changed: impl FnOnce(&mut Value) + Send,
    ) -> Result<()> {
        let path = format!("/api/playlist/{}", escape::component(playlist_id));
        let mut playlist: Value = self.item(&path).await?;
        changed(&mut playlist);
        self.change(reqwest::Method::PUT, &path, Some(&playlist))
            .await
    }

    /// Stars or unstars whatever the query names, through the `rest` route the server's own ui
    /// changes a star with.
    async fn change_saved(&self, saved: bool, query: &str) -> Result<()> {
        let route = match saved {
            true => format!("star?{query}"),
            false => format!("unstar?{query}"),
        };
        let url = self.rest_of(&route)?;
        self.ask(&url).await
    }

    /// A `rest` call that changes something, which the server answers with a status rather than
    /// with anything the app reads.
    async fn ask(&self, url: &str) -> Result<()> {
        self.inner
            .http
            .get(url)
            .send()
            .await
            .context("cannot reach the navidrome server")?
            .error_for_status()
            .context("the navidrome server refused the request")?;
        Ok(())
    }

    /// Opens the audio of a track and answers once the response headers are in; the body is
    /// still on its way.
    pub async fn open_stream(&self, track_id: &str) -> Result<reqwest::Response> {
        let url = self
            .stream_url(track_id)
            .context("cannot build the stream url")?;
        self.inner
            .http
            .get(url)
            .send()
            .await
            .context("cannot stream the track")?
            .error_for_status()
            .context("the server refused the stream")
    }

    /// The url of a track's audio, signed like every other `rest` route.
    pub fn stream_url(&self, track_id: &str) -> Option<String> {
        let base = self.rest("stream")?;
        Some(format!("{base}&id={}", escape::component(track_id)))
    }

    /// The length and ReplayGain the server records for a track, whichever it has. The track
    /// gain wins, and the album gain stands in when that is all the file carries.
    pub async fn details(&self, track_id: &str) -> Details {
        let Ok(song) = self.song_source(track_id).await else {
            return Details::default();
        };
        let duration = Some(song.duration)
            .filter(|seconds| *seconds > 0.0)
            .map(wire::length);
        let (gain, peak) = match (song.rg_track_gain, song.rg_album_gain) {
            (Some(track), _) => (Some(track), song.rg_track_peak),
            (None, album) => (album, song.rg_album_peak),
        };
        let peak = peak.map(|peak| peak as f32);
        let loudness = gain.map(|gain| Loudness::replay_gain(gain as f32, peak));
        Details { duration, loudness }
    }

    /// The lyrics the server holds for a track, which are the file's own tag text and take the
    /// same path to the screen a local file's lyrics take.
    pub async fn lyrics(&self, track_id: &str) -> Result<Option<Lyrics>> {
        let song = self.song_source(track_id).await?;
        Ok(wire::lyrics(&song.lyrics))
    }
}

#[async_trait]
impl MusicApi for NavidromeClient {
    fn share_url(&self, _kind: MediaKind, _id: &str) -> Option<String> {
        None
    }

    /// The account the session was opened with. The native api answers no route with the signed
    /// in user for a plain listener, so the name comes from sign-in and a keepalive call stands
    /// in for the server being there at all.
    async fn profile(&self) -> Result<UserProfile> {
        self.get("/api/keepalive/")
            .await
            .context("cannot reach the navidrome server")?;
        let session = self.session();
        Ok(wire::profile(&session.id, &session.name))
    }

    async fn artist(&self, artist_id: &str) -> Result<Artist> {
        let detail = self.artist_detail(artist_id).await?;
        let cover_large = self.artist_cover(&detail, artist_id);
        let biography = held(&detail.biography);
        let top_tracks = self.top_tracks(artist_id).await;
        let albums = self.artist_albums(artist_id).await;
        Ok(Artist {
            name: detail.name,
            cover_large,
            biography,
            monthly_listeners: None,
            top_tracks,
            albums,
        })
    }

    async fn artist_profile(&self, artist_id: &str) -> Result<ArtistProfile> {
        let detail = self.artist_detail(artist_id).await?;
        let cover_large = self.artist_cover(&detail, artist_id);
        Ok(ArtistProfile {
            name: detail.name,
            cover_large,
            biography: held(&detail.biography),
        })
    }

    async fn artist_images(&self, ids: Vec<String>) -> Result<HashMap<String, String>> {
        let mut tasks = JoinSet::new();
        for id in ids.into_iter().take(PORTRAIT_LIMIT) {
            let client = self.clone();
            tasks.spawn(async move {
                let detail = client.artist_detail(&id).await.ok()?;
                let cover = client.artist_cover(&detail, &id)?;
                Some((id, cover))
            });
        }
        let mut images = HashMap::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(Some((id, image))) = result {
                images.insert(id, image);
            }
        }
        Ok(images)
    }

    /// The tracks the account starred, which on a `Shape::Catalog` provider feed the hearts and
    /// the favorites filter rather than the library itself.
    async fn saved_tracks(&self) -> Result<Vec<Track>> {
        every(self.saved_tracks_paged().await?).await
    }

    async fn saved_tracks_paged(&self) -> Result<Pages<Track>> {
        let query = "/api/song?starred=true&_sort=starredAt&_order=DESC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::song))
    }

    async fn all_tracks(&self) -> Result<Vec<Track>> {
        every(self.all_tracks_paged().await?).await
    }

    async fn all_tracks_paged(&self) -> Result<Pages<Track>> {
        let query = "/api/song?_sort=title&_order=ASC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::song))
    }

    async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<()> {
        let query = format!("id={}", escape::component(track_id));
        self.change_saved(saved, &query)
            .await
            .with_context(|| format!("cannot change the star for {track_id}"))
    }

    async fn track(&self, track_id: &str) -> Result<Track> {
        Ok(self.song(self.song_source(track_id).await?))
    }

    async fn track_playcount(&self, track_id: &str) -> Result<Option<u64>> {
        let Ok(song) = self.song_source(track_id).await else {
            return Ok(None);
        };
        match song.play_count {
            0 => Ok(None),
            count => Ok(Some(count)),
        }
    }

    /// Tells the server where a track is, through the `reportPlayback` call navidrome's own ui
    /// plays with. Scrobbling is left to `played`, so a position never counts as a listen.
    async fn report(&self, track_id: &str, report: Report, position: Duration) -> Result<()> {
        let state = match report {
            Report::Playing => "playing",
            Report::Paused => "paused",
            Report::Stopped => "stopped",
        };
        let id = escape::component(track_id);
        let millis = i64::try_from(position.as_millis()).unwrap_or(i64::MAX);
        let route = format!(
            "reportPlayback?mediaId={id}&mediaType=song&positionMs={millis}&state={state}",
        );
        let route = format!("{route}&ignoreScrobble=true");
        let url = self.rest_of(&route)?;
        self.ask(&url)
            .await
            .with_context(|| format!("cannot report {track_id} as {state}"))
    }

    /// Navidrome takes the start of the listen in milliseconds since the epoch.
    async fn played(&self, track_id: &str, at: SystemTime) -> Result<()> {
        let millis = at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let time = i64::try_from(millis).unwrap_or(i64::MAX);
        let id = escape::component(track_id);
        let route = format!("scrobble?id={id}&submission=true&time={time}");
        let url = self.rest_of(&route)?;
        self.ask(&url)
            .await
            .with_context(|| format!("cannot record a play of {track_id}"))
    }

    /// The tracks the account played last, newest first, across every device that reports to
    /// the server.
    async fn recently_played(&self) -> Result<Vec<Track>> {
        let query = format!("/api/song?_sort=playDate&_order=DESC&_start=0&_end={RECENT_COUNT}");
        let rows: Vec<wire::Song> = self.item(&query).await?;
        Ok(rows.into_iter().map(|row| self.song(row)).collect())
    }

    async fn playlists(&self) -> Result<Vec<Playlist>> {
        let query = "/api/playlist?_sort=name&_order=ASC";
        let pages = self.paged(LIBRARY_PAGE, query, Self::playlist_of);
        every(pages).await
    }

    async fn create_playlist(&self, name: &str) -> Result<String> {
        let body = serde_json::json!({ "name": name });
        let answer = self
            .request(reqwest::Method::POST, "/api/playlist", Some(&body))
            .await
            .context("cannot create the playlist")?;
        let made: wire::Playlist = self.read(answer).await?;
        Ok(made.id)
    }

    async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        self.change_playlist(playlist_id, |playlist| {
            playlist["name"] = Value::from(name);
        })
        .await
        .context("cannot rename the playlist")
    }

    async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        let path = format!("/api/playlist/{}", escape::component(playlist_id));
        let answer = self
            .request(reqwest::Method::DELETE, &path, None)
            .await
            .context("cannot delete the playlist")?;
        drop(answer);
        Ok(())
    }

    async fn remove_playlist_from_library(&self, _playlist_id: &str) -> Result<()> {
        Ok(())
    }

    async fn add_playlist_to_library(&self, _playlist_id: &str) -> Result<()> {
        Ok(())
    }

    async fn set_playlist_public(&self, playlist_id: &str, public: bool) -> Result<()> {
        self.change_playlist(playlist_id, move |playlist| {
            playlist["public"] = Value::from(public);
        })
        .await
        .context("cannot change the playlist visibility")
    }

    async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        let path = format!("/api/playlist/{}/tracks", escape::component(playlist_id));
        let body = serde_json::json!({ "ids": [track_id] });
        self.change(reqwest::Method::POST, &path, Some(&body))
            .await
            .context("cannot add the track to the playlist")
    }

    /// The tracks route takes the ids to drop as query parameters, which is how the server's
    /// own ui removes a row.
    async fn remove_track_from_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        let id = escape::component(track_id);
        let list = escape::component(playlist_id);
        let path = format!("/api/playlist/{list}/tracks?id={id}");
        let answer = self
            .request(reqwest::Method::DELETE, &path, None)
            .await
            .context("cannot remove the track from the playlist")?;
        drop(answer);
        Ok(())
    }

    async fn saved_albums(&self) -> Result<Vec<Album>> {
        every(self.saved_albums_paged().await?).await
    }

    async fn saved_albums_paged(&self) -> Result<Pages<Album>> {
        let query = "/api/album?starred=true&_sort=starredAt&_order=DESC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::album_of))
    }

    async fn all_albums(&self) -> Result<Vec<Album>> {
        every(self.all_albums_paged().await?).await
    }

    async fn all_albums_paged(&self) -> Result<Pages<Album>> {
        let query = "/api/album?_sort=name&_order=ASC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::album_of))
    }

    async fn set_album_saved(&self, album_id: &str, saved: bool) -> Result<()> {
        let query = format!("albumId={}", escape::component(album_id));
        self.change_saved(saved, &query)
            .await
            .with_context(|| format!("cannot change the star for album {album_id}"))
    }

    async fn saved_artists(&self) -> Result<Vec<SavedArtist>> {
        every(self.saved_artists_paged().await?).await
    }

    async fn saved_artists_paged(&self) -> Result<Pages<SavedArtist>> {
        let query = "/api/artist?starred=true&_sort=starredAt&_order=DESC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::artist_of))
    }

    async fn all_artists(&self) -> Result<Vec<SavedArtist>> {
        every(self.all_artists_paged().await?).await
    }

    async fn all_artists_paged(&self) -> Result<Pages<SavedArtist>> {
        let query = "/api/artist?_sort=name&_order=ASC";
        Ok(self.paged(LIBRARY_PAGE, query, Self::artist_of))
    }

    async fn set_artist_saved(&self, artist_id: &str, saved: bool) -> Result<()> {
        let query = format!("artistId={}", escape::component(artist_id));
        self.change_saved(saved, &query)
            .await
            .with_context(|| format!("cannot change the star for artist {artist_id}"))
    }

    async fn album(&self, album_id: &str) -> Result<AlbumDetail> {
        let path = format!("/api/album/{}", escape::component(album_id));
        let source = self
            .item::<wire::Album>(&path)
            .await
            .with_context(|| format!("cannot load the album {album_id}"))?;
        let album = self.album_of(source);
        let tracks = self.album_tracks(album_id).await?;
        Ok(AlbumDetail { album, tracks })
    }

    async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        let album = escape::component(album_id);
        let query = format!("/api/song?album_id={album}&_sort=trackNumber&_order=ASC");
        let pages = self.paged(LIBRARY_PAGE, &query, Self::song);
        let mut tracks = every(pages).await?;
        // the server lists an album in track order, and an album that spans several discs is
        // still heard disc by disc
        tracks.sort_by(|a, b| {
            (a.disc_number, a.track_number)
                .cmp(&(b.disc_number, b.track_number))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(tracks)
    }

    /// The rail an album page draws once its tracks are up: what else the same artist has. The
    /// native api lists neither related releases nor similar artists, so the rail carries only
    /// what it can.
    async fn album_catalogue(
        &self,
        album_id: &str,
        artist_id: Option<&str>,
    ) -> Result<AlbumCatalogue> {
        let Some(artist_id) = artist_id else {
            return Ok(AlbumCatalogue::default());
        };
        let artist = escape::component(artist_id);
        let end = SUGGESTIONS + 1;
        let query =
            format!("/api/album?artist_id={artist}&_sort=maxYear&_order=DESC&_start=0&_end={end}",);
        let rows: Vec<wire::Album> = self.item(&query).await.unwrap_or_default();
        let also_like = rows
            .into_iter()
            .map(|row| self.album_of(row))
            .filter(|album| album.id != album_id)
            .take(SUGGESTIONS)
            .collect();
        Ok(AlbumCatalogue {
            also_like,
            similar: Vec::new(),
        })
    }

    async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        let path = format!("/api/playlist/{}", escape::component(playlist_id));
        let source = self
            .item::<wire::Playlist>(&path)
            .await
            .with_context(|| format!("cannot load the playlist {playlist_id}"))?;
        let mut playlist = self.playlist_of(source);
        let tracks = self.playlist_tracks(playlist_id).await?;
        if playlist.track_count == 0 {
            playlist.track_count = tracks.len() as u32;
        }
        Ok(PlaylistDetail {
            playlist,
            tracks,
            continuation: None,
        })
    }

    async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let path = format!("/api/playlist/{}/tracks", escape::component(playlist_id));
        let pages = self.paged(LIBRARY_PAGE, &path, Self::entry);
        every(pages).await
    }

    async fn playlist_covers(&self, playlist_id: &str, wanted: usize) -> Result<Vec<String>> {
        let tracks = self.playlist_tracks(playlist_id).await?;
        Ok(distinct_covers(&tracks, wanted))
    }

    /// The station a track seeds. The native api has no route that lists what sounds like a
    /// track, so the station is the rest of the artist's library shuffled, which is the closest
    /// the server can answer; a track with no artist seeds the whole library, since a filter
    /// with nothing in it is ignored.
    async fn track_radio(
        &self,
        track_id: &str,
        _from: Option<&str>,
    ) -> Result<(Vec<Track>, Option<String>)> {
        let seed = self.song_source(track_id).await?;
        let artist = escape::component(&seed.artist_id);
        let sort = format!("/api/song?artist_id={artist}&_sort=random");
        let query = format!("{sort}&_start=0&_end={RADIO_COUNT}");
        let rows: Vec<wire::Song> = self
            .item(&query)
            .await
            .context("cannot load a radio station")?;
        let mut tracks: Vec<Track> = rows.into_iter().map(|row| self.song(row)).collect();
        tracks.retain(|track| track.id.as_deref() != Some(track_id));
        Ok((tracks, None))
    }

    async fn search(&self, query: &str) -> Result<Vec<Track>> {
        let path = format!(
            "/api/song?title={}&_sort=title&_order=ASC&_start=0&_end={SEARCH_SONGS}",
            escape::component(query),
        );
        let rows: Vec<wire::Song> = self.item(&path).await.context("cannot search")?;
        Ok(rows.into_iter().map(|row| self.song(row)).collect())
    }

    async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        let path = format!(
            "/api/album?name={}&_sort=name&_order=ASC&_start=0&_end={SEARCH_ALBUMS}",
            escape::component(query),
        );
        let rows: Vec<wire::Album> = self.item(&path).await.context("cannot search albums")?;
        Ok(rows.into_iter().map(|row| self.album_of(row)).collect())
    }

    /// Playlists are few enough to filter here: the server's own listing takes no search.
    async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        let needle = query.to_lowercase();
        Ok(self
            .playlists()
            .await?
            .into_iter()
            .filter(|playlist| playlist.name.to_lowercase().contains(&needle))
            .collect())
    }

    async fn home(&self) -> Result<HomeFeed> {
        let random = self.random_songs(HOME_SONGS).await;
        let newest = self
            .albums_sorted("recently_added", "DESC", HOME_ALBUMS)
            .await;
        let played = self.albums_sorted("playCount", "DESC", HOME_ALBUMS).await;

        let mut sections = Vec::new();
        if !newest.is_empty() {
            sections.push(GenreSection {
                title: "home-newest-albums".to_owned(),
                items: newest.into_iter().map(GenreItem::Album).collect(),
            });
        }
        if !played.is_empty() {
            sections.push(GenreSection {
                title: "home-most-played-albums".to_owned(),
                items: played.into_iter().map(GenreItem::Album).collect(),
            });
        }

        Ok(HomeFeed {
            listen_again: random
                .iter()
                .take(10)
                .cloned()
                .map(GenreItem::Track)
                .collect(),
            quick_picks: Some(random.into_iter().take(15).collect()),
            sections,
        })
    }

    async fn genres(&self) -> Result<Vec<Genre>> {
        let pages = self.paged(
            LIBRARY_PAGE,
            "/api/genre?_sort=name&_order=ASC",
            |_, row| wire::genre(row),
        );
        every(pages).await
    }

    /// One genre's albums, newest first. A genre the server lists by id alone is named by the
    /// route that holds it.
    async fn genre(&self, genre_id: &str) -> Result<GenreDetail> {
        let id = escape::component(genre_id);
        let path = format!("/api/genre/{id}");
        let source = self
            .item::<wire::Genre>(&path)
            .await
            .with_context(|| format!("cannot load the genre {genre_id}"))?;
        let name = source.name;
        let query = format!("/api/album?genre_id={id}&_sort=maxYear&_order=DESC");
        let rows = albums_of_genre(self, &query).await?;
        let items: Vec<GenreItem> = rows.into_iter().map(GenreItem::Album).collect();
        Ok(GenreDetail {
            name: name.clone(),
            sections: match items.is_empty() {
                true => Vec::new(),
                false => vec![GenreSection { title: name, items }],
            },
        })
    }
}

/// Every row of a listing that arrives a page at a time, for the callers that want it in one
/// go.
async fn every<T>(mut pages: Pages<T>) -> Result<Vec<T>> {
    let mut rows = Vec::new();
    while let Some(page) = pages.recv().await {
        rows.extend(page?.items);
    }
    Ok(rows)
}

/// Up to [`GENRE_ALBUMS`] albums of a genre, which is as much of one as a page draws.
async fn albums_of_genre(client: &NavidromeClient, query: &str) -> Result<Vec<Album>> {
    let asked = format!("{query}&_start=0&_end={GENRE_ALBUMS}");
    let rows: Vec<wire::Album> = client.item(&asked).await?;
    Ok(rows.into_iter().map(|row| client.album_of(row)).collect())
}

/// How many rows the server says a listing holds, from the header it keeps the count in.
fn total_of(answer: &reqwest::Response) -> Option<usize> {
    answer
        .headers()
        .get(TOTAL_COUNT)
        .and_then(|count| count.to_str().ok())
        .and_then(|count| count.parse::<usize>().ok())
}

/// A text the server holds as nothing at all, which the app draws as missing.
fn held(text: &str) -> Option<String> {
    let text = text.trim();
    match text.is_empty() {
        true => None,
        false => Some(text.to_owned()),
    }
}
