use std::time::Duration;

use serde::Deserialize;

use crate::lyrics::lrc;
use crate::models;
use crate::models::ReleaseType;

/// A song, as `/api/song` hands it over. Every field the server leaves out comes back empty,
/// which is how it reports a count of none or a tag the file never carried.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Song {
    pub id: String,
    pub title: String,
    pub album: String,
    pub album_id: String,
    pub artist: String,
    pub artist_id: String,
    pub album_artist: String,
    pub album_artist_id: String,
    pub track_number: u32,
    pub disc_number: u32,
    pub duration: f64,
    pub year: i32,
    pub play_count: u64,
    pub created_at: String,
    /// The lyrics the file carries, as the server parsed them out of its own tags.
    pub lyrics: String,
    pub explicit_status: String,
    pub missing: bool,
    pub has_cover_art: bool,
    pub genres: Vec<Named>,
    pub tags: Tags,
    pub participants: Participants,
    pub starred: Option<bool>,
    /// What the file's own ReplayGain tags hold, which is all the server knows about how loud
    /// a track is.
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
}

/// A release, as `/api/album` hands it over.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub name: String,
    pub album_artist: String,
    pub album_artist_id: String,
    pub max_year: i32,
    pub min_year: i32,
    pub max_original_year: i32,
    pub date: String,
    pub compilation: bool,
    pub song_count: u32,
    pub mbz_album_type: String,
    pub genres: Vec<Named>,
    pub tags: Tags,
    pub participants: Participants,
    pub created_at: String,
    pub starred: Option<bool>,
}

/// An artist, as `/api/artist` hands one over. The list and the detail route carry the same
/// fields; only the picture and the biography are missing from a list.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub image_absent: bool,
    pub biography: String,
    pub small_image_url: String,
    pub medium_image_url: String,
    pub large_image_url: String,
    pub created_at: Option<String>,
}

/// A playlist, as `/api/playlist` hands one over. The tracks are not among these fields: the
/// server lists them apart, at `/api/playlist/{id}/tracks`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub owner_id: String,
    pub owner_name: String,
    pub public: bool,
    pub song_count: u32,
    pub sync: bool,
    pub updated_at: Option<String>,
}

/// One row of a playlist: the song, under the id the playlist gives its own place in itself.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PlaylistEntry {
    pub media_file_id: String,
    #[serde(flatten)]
    pub song: Song,
}

/// A genre, as `/api/genre` hands one over.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Genre {
    pub id: String,
    pub name: String,
}

/// Something the server names with an id of its own, such as a genre or an artist.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Named {
    pub id: String,
    pub name: String,
}

/// The artists the server credits on a song or a release, by role.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Participants {
    pub artist: Vec<Named>,
    pub albumartist: Vec<Named>,
}

/// The file's own tags, as the server imported them: every value a tag holds, by tag name.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Tags {
    pub genre: Vec<String>,
    pub language: Vec<String>,
    pub releasetype: Vec<String>,
}

pub fn track(source: Song, cover: Option<String>) -> models::Track {
    let (artists, artist_refs) = artists_of(
        &source.participants,
        false,
        (&source.artist, &source.artist_id),
    );
    let tags = genres(&source);
    let languages = source.tags.language.clone();
    models::Track {
        id: Some(source.id),
        name: source.title,
        // a file the server cannot find is not one it can hand over
        playable: !source.missing,
        artists,
        artist_refs,
        album: source.album,
        album_id: unless_empty(source.album_id),
        cover,
        duration: length(source.duration),
        added_at: moment(Some(&source.created_at)),
        added_by: None,
        playcount: match source.play_count {
            0 => None,
            count => Some(count),
        },
        popularity: 0,
        explicit: source.explicit_status == "e",
        track_number: source.track_number,
        disc_number: source.disc_number.max(1),
        tags,
        languages,
        credits: Vec::new(),
    }
}

pub fn album(source: Album, cover: Option<String>, cover_large: Option<String>) -> models::Album {
    let (artists, artist_refs) = artists_of(
        &source.participants,
        true,
        (&source.album_artist, &source.album_artist_id),
    );
    // what the server itself calls the year of a release: the year it was first released, and
    // failing that the year of the files it holds
    let year = match source.max_original_year {
        0 => source.max_year,
        original => original,
    };
    let release_date = match source.date.is_empty() {
        true => match year {
            0 => String::new(),
            _ => year.to_string(),
        },
        false => source.date.clone(),
    };
    models::Album {
        id: source.id,
        name: source.name,
        artists,
        artist_refs,
        cover,
        cover_large,
        release_type: release_type(&source),
        year,
        track_count: source.song_count,
        release_date,
        // the native api keeps no record label and no copyright line
        label: String::new(),
        copyrights: Vec::new(),
        added_at: moment(Some(&source.created_at)),
    }
}

pub fn saved_artist(source: &Artist, cover: Option<String>) -> models::SavedArtist {
    models::SavedArtist {
        id: source.id.clone(),
        name: source.name.clone(),
        cover,
        added_at: moment(source.created_at.as_deref()),
    }
}

pub fn genre(source: Genre) -> models::Genre {
    models::Genre {
        id: source.id,
        name: source.name,
        cover: None,
    }
}

pub fn playlist(source: &Playlist, cover: Option<String>, username: &str) -> models::Playlist {
    models::Playlist {
        id: source.id.clone(),
        name: source.name.clone(),
        owner: source.owner_name.clone(),
        owner_id: source.owner_id.clone(),
        owned: !source.owner_name.is_empty() && source.owner_name == username,
        collaborative: false,
        blend: false,
        public: source.public,
        cover,
        track_count: source.song_count,
        modified_at: moment(source.updated_at.as_deref()),
    }
}

/// The account, from what sign-in answered rather than from a route of its own: the native api
/// has none that a plain listener may read.
pub fn profile(id: &str, name: &str) -> models::UserProfile {
    let name = name.trim();
    models::UserProfile {
        id: id.to_owned(),
        display_name: match name.is_empty() {
            true => id.to_owned(),
            false => name.to_owned(),
        },
        avatar: None,
    }
}

/// The sheet of a track, from what the native api hands over in its `lyrics` field: the list
/// navidrome keeps for the file, whose lines still hold the text as it was written, the
/// per-character marks of an LRC included. It goes back out as LRC and takes the same path a
/// local file's own lyrics take, so a stamped sheet arrives timed and worded as it was.
pub fn lyrics(held: &str) -> Option<models::Lyrics> {
    let held = held.trim();
    if held.is_empty() || held == "[]" {
        return None;
    }
    let Ok(sheets) = serde_json::from_str::<Vec<Sheet>>(held) else {
        // a server that keeps the raw tag text instead hands that over, and it is already what
        // the reader takes
        return read(held);
    };
    let sheet = sheets
        .iter()
        .find(|sheet| sheet.is_main() && !sheet.line.is_empty())?;
    if !sheet.synced {
        return Some(models::Lyrics::plain(sheet.plain()));
    }
    if let Some(lyrics) = read(&sheet.lrc()) {
        return Some(lyrics);
    }
    Some(models::Lyrics::plain(sheet.plain()))
}

/// One sheet of lyrics, as navidrome keeps it inside a song's `lyrics` field.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Sheet {
    /// What the sheet is for: a translation or a pronunciation is not the song itself.
    kind: String,
    synced: bool,
    /// How far the marks are out, in milliseconds, added to every line.
    offset: Option<i64>,
    line: Vec<Line>,
}

impl Sheet {
    /// Whether this sheet is the song rather than something written about it. A sheet with no
    /// kind at all is the song.
    fn is_main(&self) -> bool {
        matches!(self.kind.as_str(), "" | "main")
    }

    /// The lines as LRC, each carrying the sheet's own mark and the moment it starts.
    fn lrc(&self) -> String {
        let offset = self.offset.unwrap_or(0);
        self.line
            .iter()
            .map(|line| {
                let at = offset.saturating_add(line.start.unwrap_or(0));
                format!("[{}]{}", stamp(at), line.value.trim())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The lines with no marks on them, for a sheet that was never timed.
    fn plain(&self) -> String {
        self.line
            .iter()
            .map(|line| line.value.trim())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One line of a sheet, with the text the file wrote, per-character marks and all.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Line {
    start: Option<i64>,
    value: String,
}

/// The lyrics the given text holds, timed when they carry a mark and plain together when they
/// do not.
fn read(text: &str) -> Option<models::Lyrics> {
    let lines = lrc::parse(text);
    match lines.is_empty() {
        true => match text.trim().is_empty() {
            true => None,
            false => Some(models::Lyrics::plain(text.trim())),
        },
        false => Some(models::Lyrics::Synced {
            lines: lines.into(),
        }),
    }
}

/// A time the way LRC writes one.
fn stamp(millis: i64) -> String {
    let millis = millis.max(0);
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1_000 % 60,
        millis % 1_000
    )
}

/// The credited artists of a song or a release: the roles the server filled in, else the plain
/// name and id beside them, which older servers set alone. The joined names are what a row
/// shows, and the refs are what an artist page can follow.
fn artists_of(
    participants: &Participants,
    release: bool,
    plain: (&str, &str),
) -> (String, Vec<models::ArtistRef>) {
    let (name, id) = plain;
    let (mine, other) = match release {
        true => (&participants.albumartist, &participants.artist),
        false => (&participants.artist, &participants.albumartist),
    };
    let listed = match mine.is_empty() {
        true => other,
        false => mine,
    };
    if listed.is_empty() {
        let id = unless_empty(id.to_owned());
        let refs = match (name.is_empty(), id) {
            (true, _) => Vec::new(),
            (false, id) => vec![models::ArtistRef {
                name: name.to_owned(),
                id,
            }],
        };
        return (name.to_owned(), refs);
    }
    let joined = listed
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let refs = listed
        .iter()
        .map(|artist| models::ArtistRef {
            name: artist.name.clone(),
            id: unless_empty(artist.id.clone()),
        })
        .collect();
    (joined, refs)
}

/// The kind of release an album is, from the release types the file's own tags carry and the
/// type musicbrainz holds, either of which may name it, with the compilation flag on top.
fn release_type(source: &Album) -> ReleaseType {
    let release = &source.tags.releasetype;
    let mut types: Vec<&str> = release.iter().map(String::as_str).collect();
    if !source.mbz_album_type.is_empty() {
        types.push(&source.mbz_album_type);
    }
    ReleaseType::from_musicbrainz(types, source.compilation)
}

/// The genres a song carries, which the server lists apart from the tags it imported.
fn genres(source: &Song) -> Vec<String> {
    let listed: Vec<String> = source.genres.iter().map(genre_name).collect();
    match listed.is_empty() {
        true => source.tags.genre.clone(),
        false => listed,
    }
}

/// The name of a genre the server names with an id beside it.
fn genre_name(genre: &Named) -> String {
    genre.name.clone()
}

/// Seconds the server reports for a length, as a duration. A missing or nonsensical number is
/// nothing at all rather than a panic, since only the server knows how long a file runs.
pub fn length(seconds: f64) -> Duration {
    match seconds.is_finite() && seconds > 0.0 {
        true => Duration::try_from_secs_f64(seconds).unwrap_or_default(),
        false => Duration::ZERO,
    }
}

/// An ISO-8601 stamp as seconds since the epoch, which is what a library column sorts and
/// draws. The server writes them in whatever offset it keeps, and one it cannot read is
/// nothing rather than a guess.
fn moment(stamp: Option<&str>) -> Option<i64> {
    let at = stamp?.parse::<jiff::Timestamp>().ok()?;
    Some(at.as_second())
}

/// Whether the server said anything at all, which it reports by saying nothing.
fn unless_empty(value: String) -> Option<String> {
    match value.is_empty() {
        true => None,
        false => Some(value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn song() -> Song {
        serde_json::from_value(json!({
            "id": "3INJ08VersI4pgzzu17N5N",
            "title": "为难",
            "album": "张靓颖@音乐",
            "albumId": "0wMBuefxNY0ilQDjGmikcI",
            "artistId": "5rcAfvtBqvfZz0uv8uEat4",
            "artist": "张靓颖",
            "albumArtistId": "5rcAfvtBqvfZz0uv8uEat4",
            "albumArtist": "张靓颖",
            "hasCoverArt": true,
            "trackNumber": 6,
            "discNumber": 0,
            "year": 2008,
            "duration": 229.8,
            "playCount": 12,
            "explicitStatus": "e",
            "missing": false,
            "genres": [{ "id": "2nTcfdpCh1gH8vcTUnqTCP", "name": "流行·深情" }],
            "tags": { "genre": ["流行"], "language": ["中文"] },
            "participants": {
                "artist": [{ "id": "5rcAfvtBqvfZz0uv8uEat4", "name": "张靓颖", "missing": false }],
            },
            "rgTrackGain": -6.5,
            "rgTrackPeak": 0.98,
            "starred": true,
            "createdAt": "2026-06-15T14:27:31.14828202Z",
        }))
        .expect("the server's own song shape")
    }

    #[test]
    fn a_song_becomes_a_track() {
        let track = track(
            song(),
            Some("http://host/rest/getCoverArt?id=mf-x".to_owned()),
        );

        assert_eq!(track.id.as_deref(), Some("3INJ08VersI4pgzzu17N5N"));
        assert_eq!(track.name, "为难");
        assert!(track.playable);
        assert_eq!(track.artists, "张靓颖");
        assert_eq!(track.artist_refs.len(), 1);
        assert_eq!(
            track.artist_refs[0].id.as_deref(),
            Some("5rcAfvtBqvfZz0uv8uEat4")
        );
        assert_eq!(track.album, "张靓颖@音乐");
        assert_eq!(track.album_id.as_deref(), Some("0wMBuefxNY0ilQDjGmikcI"));
        assert_eq!(track.duration, Duration::from_millis(229_800));
        assert_eq!(track.playcount, Some(12));
        assert!(track.explicit);
        assert_eq!(track.track_number, 6);
        // the server reports a missing disc as 0, and the app counts discs from one
        assert_eq!(track.disc_number, 1);
        assert_eq!(track.tags, vec!["流行·深情"]);
        assert_eq!(track.languages, vec!["中文"]);
        assert_eq!(track.added_at, Some(1_781_533_651));
    }

    #[test]
    fn a_song_without_play_counts_or_participants_falls_back_to_its_own_name() {
        let source = serde_json::from_value(json!({
            "id": "abc",
            "title": "Untitled",
            "artist": "Someone",
            "artistId": "xyz",
            "genres": [],
            "tags": { "genre": ["Rock"] },
        }))
        .expect("a song with the fields the server leaves out");

        let track = track(source, None);

        assert_eq!(track.playcount, None);
        assert!(!track.explicit);
        assert_eq!(track.artist_refs[0].name, "Someone");
        assert_eq!(track.tags, vec!["Rock"]);
        assert_eq!(track.duration, Duration::ZERO);
        assert_eq!(track.added_at, None);
    }

    #[test]
    fn a_release_becomes_an_album() {
        let album = album(
            serde_json::from_value(json!({
                "id": "66bGjYrzC4MVG7Njc6vLWa",
                "name": "Day and Night",
                "albumArtistId": "0FGrH6nTS3aSUiSX72yA9e",
                "albumArtist": "Carly Rae Jepsen",
                "maxYear": 2026,
                "minYear": 2026,
                "date": "2026-09-29",
                "compilation": false,
                "songCount": 20,
                "mbzAlbumType": "EP",
                "genres": [{ "id": "1JiUpwE6UBPUcj62e3KAXm", "name": "流行" }],
                "tags": { "releasetype": ["album"] },
                "participants": {
                    "albumartist": [{
                        "id": "0FGrH6nTS3aSUiSX72yA9e",
                        "name": "Carly Rae Jepsen",
                        "missing": false,
                    }],
                },
                "createdAt": "2026-09-29T14:35:20.496386827Z",
            }))
            .expect("the server's own album shape"),
            Some("http://host/rest/getCoverArt?id=al-66bGjYrzC4MVG7Njc6vLWa".to_owned()),
            None,
        );

        assert_eq!(album.name, "Day and Night");
        assert_eq!(album.artists, "Carly Rae Jepsen");
        assert_eq!(
            album.artist_refs[0].id.as_deref(),
            Some("0FGrH6nTS3aSUiSX72yA9e")
        );
        assert_eq!(album.year, 2026);
        assert_eq!(album.release_date, "2026-09-29");
        assert_eq!(album.track_count, 20);
        // the release type of an EP outranks the album tag beside it
        assert_eq!(album.release_type, ReleaseType::Ep);
        assert_eq!(album.added_at, Some(1_790_692_520));
    }

    #[test]
    fn an_artist_becomes_a_saved_artist() {
        let artist = saved_artist(
            &serde_json::from_value(json!({
                "imageAbsent": true,
                "id": "2h2QuJqr7QF0JRsiNi9z9l",
                "name": "+1",
                "songCount": 1,
                "albumCount": 1,
                "createdAt": "2026-06-07T18:06:56.000039703Z",
            }))
            .expect("the server's own artist shape"),
            None,
        );

        assert_eq!(artist.id, "2h2QuJqr7QF0JRsiNi9z9l");
        assert_eq!(artist.name, "+1");
        assert_eq!(artist.added_at, Some(1_780_855_616));
    }

    #[test]
    fn a_playlist_of_the_signed_in_account_reads_as_owned() {
        let source: Playlist = serde_json::from_value(json!({
            "id": "3wFqiVMFLE6fXrGvVyA9kZ",
            "name": "Morning",
            "ownerName": "Drama",
            "ownerId": "1ctciRcfouKiPJviQFMOD9",
            "public": false,
            "songCount": 12,
            "updatedAt": "2026-10-01T15:39:43Z",
        }))
        .expect("the server's own playlist shape");

        let playlist = playlist(&source, None, "Drama");
        assert!(playlist.owned);
        assert_eq!(playlist.owner_id, "1ctciRcfouKiPJviQFMOD9");
        assert_eq!(playlist.track_count, 12);
        assert_eq!(playlist.modified_at, Some(1_790_869_183));
        assert!(!playlist.public);
        assert!(!playlist(&source, None, "someone else").owned);
    }

    /// The `lyrics` field of a song that carries a per-character LRC, exactly as the server
    /// writes it back.
    fn held() -> String {
        serde_json::json!([{
            "kind": "main",
            "lang": "xxx",
            "line": [
                { "start": 0, "value": "<00:00.000>\u{4e3a}<00:00.180>\u{96be}" },
                { "start": 2530, "value": "\u{8bcd}" },
            ],
            "offset": 0,
            "synced": true,
        }])
        .to_string()
    }

    #[test]
    fn the_lyrics_of_a_song_come_back_timed_and_worded() {
        let lyrics = lyrics(&held()).expect("a sheet the server holds");

        let models::Lyrics::Synced { lines } = lyrics else {
            panic!("a synced sheet stays synced");
        };
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "为难");
        assert_eq!(lines[0].start, Duration::ZERO);
        assert_eq!(lines[1].text, "词");
        assert_eq!(lines[1].start, Duration::from_millis(2_530));
        let words = lines[0].words.as_ref().expect("the line is worded");
        assert_eq!(words.len(), 2);
        assert_eq!(words[1].text, "难");
        assert_eq!(words[1].start, Duration::from_millis(180));
    }

    #[test]
    fn a_sheet_that_was_never_timed_stays_plain() {
        let lyrics =
            lyrics("[{\"line\":[{\"value\":\"first line\"},{\"value\":\"second line\"}]}]")
                .expect("a sheet the server holds");

        assert_eq!(lyrics, models::Lyrics::plain("first line\nsecond line"));
    }

    #[test]
    fn a_song_with_no_lyrics_answers_nothing() {
        assert_eq!(lyrics(""), None);
        assert_eq!(lyrics("[]"), None);
    }

    /// A server that keeps the file's own tag text instead of the parsed list hands that over,
    /// which is what a local file's lyrics are read from too.
    #[test]
    fn raw_lrc_text_is_read_as_it_is() {
        let lyrics = lyrics("[00:10.00] first\n[00:14.50] second\n").expect("a sheet");

        let models::Lyrics::Synced { lines } = lyrics else {
            panic!("a stamped sheet is synced");
        };
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[0].start, Duration::from_secs(10));
        assert_eq!(lines[1].end, None);
    }
}
