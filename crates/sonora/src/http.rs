use std::collections::HashMap;
use std::fs::{self, FileTimes, OpenOptions};
use std::future::Future;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use gpui::http_client::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH,
    LAST_MODIFIED, RANGE,
};
use gpui::http_client::http::{HeaderValue, Method};
use gpui::http_client::{AsyncBody, HttpClient, Inner, Request, Response, Url};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

const USER_AGENT: &str = "sonora";
const CACHE_BYTES: u64 = 128 * 1024 * 1024;
const CACHE_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const CACHE_SWEEP: Duration = Duration::from_secs(60 * 60);
/// How long a connection may take to come up. A cover asked for over a link that is not answering
/// otherwise holds its place in the artwork cache for good, and every tile queued behind it stays
/// blank until the client gives up on it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a transfer may stall between two reads. This cuts off a read that has stopped rather
/// than a transfer that is slow: a cover of a few megabytes over a thin line takes as long as it
/// takes and is worth waiting for, while a connection that has already died costs frames.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The line a stored image carries ahead of its bytes, so an entry this cache wrote before it
/// kept any of that can still be read as the image it is.
const CACHE_MAGIC: &[u8] = b"sonora-cache/1\n";
/// How long the bytes of an entry the server asked to be revalidated are served without asking
/// it again. A window of none would put a request in front of every cover on the screen however
/// fast the link, and covers a page is scrolled through twice are what this saves.
const RECHECK_AFTER: Duration = Duration::from_secs(60);
/// How long that check may take before the stored bytes are drawn instead. It is short because
/// there is something to fall back on: a cover the server cannot be reached for is worth more
/// than a blank tile.
const RECHECK_TIMEOUT: Duration = Duration::from_secs(4);
/// How many covers one run remembers having checked with the server. The map only saves the
/// requests a second look at a cover would make, so it is emptied whole rather than trimmed.
const CHECKED_ITEMS: usize = 4096;

static TEMP_FILE: AtomicU64 = AtomicU64::new(0);

type Sent = Pin<Box<dyn Future<Output = Result<Response<AsyncBody>>> + Send>>;

pub struct Client {
    inner: reqwest::Client,
    handle: Handle,
    user_agent: HeaderValue,
    cache: Option<Arc<Mutex<DiskCache>>>,
}

impl Client {
    pub fn new(handle: Handle) -> Self {
        let _guard = handle.enter();
        let cache = dirs::cache_dir()
            .map(|root| root.join("sonora").join("images"))
            .and_then(|root| match DiskCache::new(root, CACHE_BYTES, CACHE_AGE) {
                Ok(cache) => Some(Arc::new(Mutex::new(cache))),
                Err(error) => {
                    log::warn!("artwork: cannot initialize the disk cache: {error}");
                    None
                }
            });
        if let Some(cache) = cache.clone() {
            handle.spawn_blocking(move || with_cache(&cache, |cache| cache.sweep()));
        }
        Self {
            inner: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                .build()
                .unwrap_or_default(),
            handle: handle.clone(),
            user_agent: HeaderValue::from_static(USER_AGENT),
            cache,
        }
    }
}

impl HttpClient for Client {
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, request: Request<AsyncBody>) -> Sent {
        let client = self.inner.clone();
        let handle = self.handle.clone();
        let cache = self.cache.clone();

        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = read(body)?;

            let fetch = handle.spawn(async move {
                let uri = parts.uri.to_string();
                let cacheable = parts.method == Method::GET
                    && !parts.headers.contains_key(AUTHORIZATION)
                    && !parts.headers.contains_key(COOKIE)
                    && !parts.headers.contains_key(RANGE);
                // What the cache holds for this url, with what the server said about it. An entry
                // the server asked to be revalidated is checked with it once it has been held for a
                // while, and its bytes are drawn as they are if that check brings back nothing
                // better: a cover the server cannot be reached for beats a blank tile.
                let mut held = match (cacheable, cache.as_ref()) {
                    (true, Some(cache)) => {
                        let cache = cache.clone();
                        let uri = uri.clone();
                        tokio::task::spawn_blocking(move || {
                            with_cache(&cache, |cache| cache.fresh(&uri)).flatten()
                        })
                        .await
                        .unwrap_or_else(|error| {
                            log::warn!("artwork: cannot read the disk cache: {error}");
                            None
                        })
                    }
                    _ => None,
                };
                let rechecking = held.as_ref().is_some_and(|held| held.due);

                if !rechecking && let Some(held) = held.take() {
                    let bytes = held.bytes;
                    return Ok::<_, anyhow::Error>((reqwest::StatusCode::OK, bytes.into()));
                }

                let mut outgoing = client.request(parts.method, &uri).headers(parts.headers);

                if let Some(held) = &held {
                    if let Some(etag) = &held.stored.etag {
                        outgoing = outgoing.header(IF_NONE_MATCH, etag.as_str());
                    }
                    if let Some(modified) = &held.stored.modified {
                        outgoing = outgoing.header(IF_MODIFIED_SINCE, modified.as_str());
                    }
                    // The check is worth only a moment: the bytes it stands for are in hand.
                    outgoing = outgoing.timeout(RECHECK_TIMEOUT);
                }
                if let Some(body) = body {
                    outgoing = outgoing.body(body);
                }

                let incoming = match outgoing.send().await {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        let Some(held) = held.take() else {
                            return Err(error.into());
                        };
                        log::debug!("artwork: {uri} was not rechecked ({error}); keeping it");
                        let bytes = held.bytes;
                        return Ok::<_, anyhow::Error>((reqwest::StatusCode::OK, bytes.into()));
                    }
                };
                let status = incoming.status();
                let is_image = incoming
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.to_ascii_lowercase().starts_with("image/"));
                let revalidate = asks_to_recheck(incoming.headers());
                let private = incoming
                    .headers()
                    .get(CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| {
                        value.split(',').map(str::trim).any(|value| {
                            value.eq_ignore_ascii_case("no-store")
                                || value.eq_ignore_ascii_case("private")
                        })
                    });
                let etag = header(&incoming, ETAG);
                let modified = header(&incoming, LAST_MODIFIED);
                let bytes = incoming.bytes().await?;

                if status == reqwest::StatusCode::NOT_MODIFIED
                    && let Some(held) = held.take()
                {
                    // The server still stands behind what it gave, so the stored bytes are good
                    // for as long as this run remembers the check: no body came over the wire to
                    // replace them, and nothing is written to disk to say they were checked.
                    if cacheable && let Some(cache) = cache.as_ref() {
                        let cache = cache.clone();
                        drop(tokio::task::spawn_blocking(move || {
                            with_cache(&cache, |cache| cache.checked(&uri));
                        }));
                    }
                    let bytes = held.bytes;
                    return Ok::<_, anyhow::Error>((reqwest::StatusCode::OK, bytes.into()));
                }
                // A cover the server has moved or cannot answer for is better drawn from the
                // cache than not drawn at all; asking for it afresh is the sweep's business.
                if !status.is_success()
                    && let Some(held) = held.take()
                {
                    log::debug!("artwork: {uri} answered {status}; keeping what is stored");
                    let bytes = held.bytes;
                    return Ok::<_, anyhow::Error>((reqwest::StatusCode::OK, bytes.into()));
                }
                if cacheable
                    && status == reqwest::StatusCode::OK
                    && is_image
                    && !private
                    && let Some(cache) = cache.as_ref()
                {
                    let cache = cache.clone();
                    let stored = Stored {
                        etag,
                        modified,
                        fetched: SystemTime::now(),
                        revalidate,
                    };
                    let written = bytes.clone();
                    drop(tokio::task::spawn_blocking(move || {
                        with_cache(&cache, |cache| cache.put(&uri, &written, &stored));
                    }));
                }
                Ok::<_, anyhow::Error>((status, bytes))
            });

            let (status, bytes) = fetch.await??;
            Ok(Response::builder()
                .status(status)
                .body(AsyncBody::from(bytes))?)
        })
    }
}

fn with_cache<T>(
    cache: &Mutex<DiskCache>,
    operation: impl FnOnce(&mut DiskCache) -> T,
) -> Option<T> {
    match cache.lock() {
        Ok(mut cache) => Some(operation(&mut cache)),
        Err(_) => {
            log::warn!("artwork: disk cache lock is poisoned");
            None
        }
    }
}

/// What a stored image has to say about itself the next time it is asked for.
#[derive(Clone)]
struct Stored {
    /// The validators the server gave the bytes with, so they can be asked about again rather
    /// than downloaded a second time.
    etag: Option<String>,
    modified: Option<String>,
    /// When the bytes arrived, which is what the freshness window is measured from.
    fetched: SystemTime,
    /// The server asked to be told about every reuse of them.
    revalidate: bool,
}

/// What the cache has for a url: the bytes, what is known about them, and whether the server asked
/// to be told about them again before they are used.
struct Held {
    bytes: Vec<u8>,
    stored: Stored,
    due: bool,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            etag: None,
            modified: None,
            fetched: SystemTime::now(),
            revalidate: false,
        }
    }
}

/// The bytes of an entry as they are written: the magic, what is known about them, a blank line
/// and the image itself.
fn encode(bytes: &[u8], stored: &Stored) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 128);
    out.extend_from_slice(CACHE_MAGIC);
    let mut line = |name: &str, value: &str| {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.push(b'\n');
    };
    if let Some(etag) = &stored.etag {
        line("etag", etag);
    }
    if let Some(modified) = &stored.modified {
        line("modified", modified);
    }
    let fetched = stored
        .fetched
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    line("fetched", &fetched);
    line(
        "revalidate",
        match stored.revalidate {
            true => "1",
            false => "0",
        },
    );
    out.push(b'\n');
    out.extend_from_slice(bytes);
    out
}

/// The image in an entry and what is known about it. A file that does not start with the magic is
/// an image this cache wrote before it kept any of that, and is read as one with nothing known:
/// it is never checked with the server, and it ages out the way it always did.
fn decode(raw: &[u8]) -> (Vec<u8>, Stored) {
    let mut stored = Stored::default();
    let Some(rest) = raw.strip_prefix(CACHE_MAGIC) else {
        return (raw.to_vec(), stored);
    };
    let at = rest
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .unwrap_or(rest.len());
    let (header, body) = match rest.len() >= at + 2 {
        true => (&rest[..at], &rest[at + 2..]),
        false => (&rest[..at], &[][..]),
    };
    for line in String::from_utf8_lossy(header).lines() {
        let Some((name, value)) = line.split_once(": ") else {
            continue;
        };
        match name {
            "etag" => stored.etag = Some(value.to_owned()),
            "modified" => stored.modified = Some(value.to_owned()),
            "fetched" => {
                stored.fetched = value
                    .parse::<u64>()
                    .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
                    .unwrap_or_else(|_| SystemTime::now());
            }
            "revalidate" => stored.revalidate = value == "1",
            _ => {}
        }
    }
    (body.to_vec(), stored)
}

struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
    max_age: Duration,
    bytes: Option<u64>,
    swept: Instant,
    /// When a cover was last checked with the server and found unchanged, by url. A check that
    /// came back `not modified` is held here rather than written into the entry, so nothing is
    /// rewritten on disk to say so and a cover scrolled past twice in one run is checked once.
    checked: HashMap<String, SystemTime>,
}

impl DiskCache {
    fn new(root: PathBuf, max_bytes: u64, max_age: Duration) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_bytes,
            max_age,
            bytes: None,
            swept: Instant::now(),
            checked: HashMap::new(),
        })
    }

    /// What the cache has for `url`: the bytes, what is known about them and whether they are due
    /// to be checked with the server. Refreshes the entry's place in the cache's own order and
    /// drops one that has been held past its age.
    fn fresh(&mut self, url: &str) -> Option<Held> {
        let path = self.path(url);
        let metadata = fs::metadata(&path).ok()?;
        let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if used.elapsed().is_ok_and(|age| age > self.max_age) {
            self.remove(&path, metadata.len());
            return None;
        }

        match fs::read(&path) {
            Ok(raw) => {
                if let Ok(file) = OpenOptions::new().write(true).open(&path) {
                    let now = SystemTime::now();
                    file.set_times(FileTimes::new().set_accessed(now).set_modified(now))
                        .ok();
                }
                let (bytes, stored) = decode(&raw);
                let due = self.due(url, &stored);
                Some(Held { bytes, stored, due })
            }
            Err(_) => {
                self.remove(&path, metadata.len());
                None
            }
        }
    }

    /// Whether an entry is due to be checked with the server: the server asked to be told about
    /// every reuse, and this one has been held past the window since it arrived or was last
    /// checked.
    fn due(&self, url: &str, stored: &Stored) -> bool {
        if !stored.revalidate {
            return false;
        }
        let since = self.checked.get(url).copied().unwrap_or(stored.fetched);
        since.elapsed().is_ok_and(|age| age >= RECHECK_AFTER)
    }

    /// Records that the server was asked about an entry and still stands behind it. Only this run
    /// remembers it: the next one checks the cover again, which is what the server asked for.
    fn checked(&mut self, url: &str) {
        if self.checked.len() >= CHECKED_ITEMS {
            self.checked.clear();
        }
        self.checked.insert(url.to_owned(), SystemTime::now());
    }

    fn put(&mut self, url: &str, bytes: &[u8], stored: &Stored) {
        let entry = encode(bytes, stored);
        if entry.len() as u64 > self.max_bytes {
            return;
        }
        if self.bytes.is_none() && self.sweep().is_none() {
            return;
        }

        let path = self.path(url);
        let previous = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let key = key(url);
        let temporary = self.root.join(format!(
            ".{key}.tmp-{}-{}",
            std::process::id(),
            TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .and_then(|mut file| std::io::Write::write_all(&mut file, &entry))
            .and_then(|()| fs::rename(&temporary, &path));
        if let Err(error) = written {
            fs::remove_file(&temporary).ok();
            log::debug!("artwork: cannot write a disk cache entry: {error}");
            return;
        }

        self.bytes = self.bytes.map(|total| {
            total
                .saturating_sub(previous)
                .saturating_add(entry.len() as u64)
        });
        if self.bytes.is_some_and(|total| total > self.max_bytes)
            || self.swept.elapsed() >= CACHE_SWEEP
        {
            self.sweep();
        }
    }

    fn sweep(&mut self) -> Option<()> {
        let now = SystemTime::now();
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.root).ok()?.flatten() {
            let path = item.path();
            if !cache_entry(&path) {
                continue;
            }
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if now.duration_since(used).is_ok_and(|age| age > self.max_age) {
                fs::remove_file(path).ok();
            } else {
                entries.push((path, used, metadata.len()));
            }
        }

        entries.sort_unstable_by_key(|(_, used, _)| *used);
        let mut bytes = entries.iter().map(|(_, _, size)| size).sum::<u64>();
        for (path, _, size) in entries {
            if bytes <= self.max_bytes {
                break;
            }
            if fs::remove_file(path).is_ok() {
                bytes = bytes.saturating_sub(size);
            }
        }
        self.bytes = Some(bytes);
        self.swept = Instant::now();
        Some(())
    }

    fn path(&self, url: &str) -> PathBuf {
        self.root.join(key(url))
    }

    fn remove(&mut self, path: &Path, size: u64) {
        if fs::remove_file(path).is_ok() {
            self.bytes = self.bytes.map(|bytes| bytes.saturating_sub(size));
        }
    }
}

fn key(url: &str) -> String {
    format!("{:x}", Sha256::digest(url.as_bytes()))
}

fn cache_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A header of an answer as owned text, for the copy the cache keeps of it.
fn header(answer: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    answer
        .headers()
        .get(&name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Whether an answer asked to be checked with the server before its bytes are used again. The
/// server is the one that knows whether the art behind a cover url has changed, and `no-cache` is
/// how it says so; a `max-age` of nought says the same thing.
fn asks_to_recheck(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').map(str::trim).any(|value| {
                value.eq_ignore_ascii_case("no-cache")
                    || value.eq_ignore_ascii_case("must-revalidate")
                    || value.eq_ignore_ascii_case("max-age=0")
            })
        })
}

fn read(body: AsyncBody) -> Result<Option<Vec<u8>>> {
    match body.0 {
        Inner::Empty => Ok(None),
        Inner::Bytes(mut cursor) => {
            let mut bytes = Vec::new();
            cursor.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Inner::AsyncReader(_) => Err(anyhow!("streaming request bodies are not supported")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temporary(PathBuf);

    impl Temporary {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sonora-image-cache-test-{}-{}",
                std::process::id(),
                TEMP_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn reads_a_cached_image_and_refreshes_its_age() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        cache.put("https://example.com/cover", b"image", &Stored::default());

        let path = cache.path("https://example.com/cover");
        let old = SystemTime::now() - Duration::from_secs(30);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();

        assert_eq!(
            cache
                .fresh("https://example.com/cover")
                .map(|held| held.bytes),
            Some(b"image".to_vec())
        );
        assert!(fs::metadata(path).unwrap().modified().unwrap() > old);
    }

    #[test]
    fn removes_expired_images() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        cache.put("expired", b"old", &Stored::default());
        let expired = cache.path("expired");
        OpenOptions::new()
            .write(true)
            .open(&expired)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(120)))
            .unwrap();
        cache.sweep();

        assert!(!expired.exists());
    }

    #[test]
    fn evicts_the_least_recently_used_image_at_the_size_limit() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 200, Duration::from_secs(60)).unwrap();
        let old_image = [b'x'; 130];
        let new_image = [b'y'; 130];
        cache.put("old", &old_image, &Stored::default());
        let old = cache.path("old");
        OpenOptions::new()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(30)))
            .unwrap();
        cache.put("new", &new_image, &Stored::default());

        assert!(!old.exists());
        assert_eq!(
            cache.fresh("new").map(|held| held.bytes),
            Some(new_image.to_vec())
        );
        assert!(cache.bytes.unwrap() <= 200);
    }

    #[test]
    fn keeps_the_validators_a_server_gave_an_image() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        let stored = Stored {
            etag: Some("\"6814b7f7f2d28017\"".to_owned()),
            modified: Some("Thu, 08 Oct 2026 07:28:14 GMT".to_owned()),
            revalidate: true,
            ..Stored::default()
        };
        cache.put("https://example.com/cover", b"image", &stored);

        let held = cache.fresh("https://example.com/cover").unwrap();

        assert_eq!(held.bytes, b"image".to_vec());
        assert_eq!(held.stored.etag.as_deref(), Some("\"6814b7f7f2d28017\""));
        assert_eq!(
            held.stored.modified.as_deref(),
            Some("Thu, 08 Oct 2026 07:28:14 GMT")
        );
        assert!(held.stored.revalidate);
        // Freshly fetched, so the window has not passed and the server is left alone.
        assert!(!held.due);
    }

    #[test]
    fn reads_an_entry_written_before_the_header() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        let path = cache.path("https://example.com/cover");
        fs::write(&path, b"image").unwrap();

        let held = cache.fresh("https://example.com/cover").unwrap();

        assert_eq!(held.bytes, b"image".to_vec());
        assert!(held.stored.etag.is_none());
        assert!(!held.stored.revalidate);
        // Nothing is known about it, so it is never checked with the server.
        assert!(!held.due);
    }

    #[test]
    fn a_server_that_asks_for_a_recheck_is_asked_once_the_window_passes() {
        let root = Temporary::new();
        let cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        let url = "https://example.com/cover";
        let old = SystemTime::now() - Duration::from_secs(2 * RECHECK_AFTER.as_secs());

        let asked = Stored {
            revalidate: true,
            fetched: old,
            ..Stored::default()
        };
        assert!(cache.due(url, &asked));

        let kept = Stored {
            revalidate: false,
            ..asked
        };
        assert!(!cache.due(url, &kept));
    }

    #[test]
    fn a_cover_checked_this_run_is_not_checked_again() {
        let root = Temporary::new();
        let mut cache = DiskCache::new(root.0.clone(), 4096, Duration::from_secs(60)).unwrap();
        let url = "https://example.com/cover";
        let stored = Stored {
            revalidate: true,
            fetched: SystemTime::now() - Duration::from_secs(2 * RECHECK_AFTER.as_secs()),
            ..Stored::default()
        };
        assert!(cache.due(url, &stored));

        cache.checked(url);

        assert!(!cache.due(url, &stored));
    }
}
