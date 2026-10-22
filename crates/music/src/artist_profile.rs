use anyhow::{Context as _, Result};
use serde::Deserialize;

const SEARCH: &str = "https://api.deezer.com/search/artist";
const DETAIL: &str = "https://api.deezer.com/artist";
const LIMIT: &str = "10";

/// What the public Deezer catalogue can add to an artist a server describes in words alone: a
/// photograph of the person rather than one of their sleeves, and how many listeners they have.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistProfile {
    pub image: Option<String>,
    pub fans: Option<u64>,
}

#[derive(Clone, Default)]
pub struct ArtistProfileSearch {
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Found<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct FoundArtist {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct Detail {
    picture_xl: Option<String>,
    picture_big: Option<String>,
    nb_fan: Option<u64>,
}

impl ArtistProfileSearch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The profile for `name`, or `None` when Deezer has no artist by that name to show. A wrong
    /// photograph is worse than none, so only a result that names the same artist is taken.
    pub async fn find(&self, name: &str) -> Result<Option<ArtistProfile>> {
        let wanted = name.trim();
        if wanted.is_empty() {
            return Ok(None);
        }
        let found: Found<FoundArtist> =
            self.get(SEARCH, &[("q", wanted), ("limit", LIMIT)]).await?;
        let Some(id) = found
            .data
            .into_iter()
            .find(|artist| same(wanted, &artist.name))
            .map(|artist| artist.id)
        else {
            return Ok(None);
        };

        let detail: Detail = self.get(&format!("{DETAIL}/{id}"), &[]).await?;
        Ok(Some(ArtistProfile {
            image: detail.picture_xl.or(detail.picture_big),
            fans: detail.nb_fan,
        }))
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        self.http
            .get(url)
            .query(query)
            .send()
            .await
            .context("cannot reach the artist catalogue")?
            .error_for_status()
            .context("the artist catalogue refused the request")?
            .json::<T>()
            .await
            .context("cannot read the artist catalogue answer")
    }
}

/// Whether `found` names `wanted`, ignoring case and the punctuation a catalogue adds. A match is
/// either the whole name or the name with something appended, so "陈奕迅" finds "陈奕迅" but
/// "陈奕迅" is never mistaken for "陈奕迅 feat. 某人".
fn same(wanted: &str, found: &str) -> bool {
    fn flat(text: &str) -> String {
        text.chars()
            .filter(|letter| letter.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    let wanted = flat(wanted);
    let found = flat(found);
    !wanted.is_empty() && !found.is_empty() && found.starts_with(&wanted)
}
