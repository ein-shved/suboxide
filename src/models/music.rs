//! Music library models.

use chrono::NaiveDateTime;
use serde::Serialize;

/// Scan-local logical identity and whole-frame/byte intervals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTrackSource {
    pub sheet_path: String,
    pub number: u32,
    pub frames: std::ops::Range<usize>,
    pub bytes: std::ops::Range<u64>,
}

/// Persisted fingerprint and sheet metadata; frame maps remain request-local.
#[derive(
    Debug, Clone, PartialEq, Eq, diesel::Queryable, diesel::QueryableByName, diesel::Selectable,
)]
#[diesel(table_name = crate::db::schema::cue_sources)]
pub struct CueSource {
    pub id: i32,
    pub sheet_path: String,
    pub cue_content: String,
    pub image_size: i64,
    pub image_modified_seconds: i64,
    pub image_modified_nanos: i32,
}

impl CueSource {
    pub fn fingerprint(&self) -> std::io::Result<crate::flac::Fingerprint> {
        Ok(crate::flac::Fingerprint {
            size: u64::try_from(self.image_size).map_err(std::io::Error::other)?,
            modified_ns: u128::try_from(self.image_modified_seconds)
                .map_err(std::io::Error::other)?
                * 1_000_000_000
                + u128::try_from(self.image_modified_nanos).map_err(std::io::Error::other)?,
        })
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, diesel::Queryable, diesel::QueryableByName, diesel::Selectable,
)]
#[diesel(table_name = crate::db::schema::cue_tracks)]
pub struct CueTrack {
    pub id: i32,
    pub cue_source_id: i32,
    pub track_number: i32,
    pub start_frame: i64,
    pub end_frame: i64,
    pub byte_start: i64,
    pub byte_end: i64,
}

/// A file path or a stable numbered entry in an external sheet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SongIdentity {
    File(String),
    Cue { sheet_path: String, number: u32 },
}

impl SongIdentity {
    #[must_use]
    pub fn from_source(path: &str, cue: Option<&CueTrackSource>) -> Self {
        cue.map_or_else(
            || Self::File(path.to_owned()),
            |cue| Self::Cue {
                sheet_path: cue.sheet_path.clone(),
                number: cue.number,
            },
        )
    }
}

/// Timestamp format used by Subsonic API responses.
pub const SUBSONIC_DATETIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

// ============================================================================
// API entity ids
// ============================================================================
//
// API-facing ids are namespaced by entity type (navidrome-style) so that ids
// never collide across tables: `mf-` song (media file), `al-` album,
// `ar-` artist, `pl-` playlist. Music folder ids stay plain integers.

/// API id for a song (media file).
#[must_use]
pub fn song_api_id(id: i32) -> String {
    format!("mf-{id}")
}

/// API id for an album.
#[must_use]
pub fn album_api_id(id: i32) -> String {
    format!("al-{id}")
}

/// API id for an artist.
#[must_use]
pub fn artist_api_id(id: i32) -> String {
    format!("ar-{id}")
}

/// API id for a playlist.
#[must_use]
pub fn playlist_api_id(id: i32) -> String {
    format!("pl-{id}")
}

/// A parsed API entity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityId {
    Song(i32),
    Album(i32),
    Artist(i32),
    Playlist(i32),
}

impl EntityId {
    /// Parse an API id string. A bare integer means a song id (the Subsonic
    /// default for endpoints like `stream`); prefixed ids are resolved by
    /// their namespace prefix.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        if let Some(rest) = id.strip_prefix("mf-") {
            return rest.parse().ok().map(Self::Song);
        }
        if let Some(rest) = id.strip_prefix("al-") {
            return rest.parse().ok().map(Self::Album);
        }
        if let Some(rest) = id.strip_prefix("ar-") {
            return rest.parse().ok().map(Self::Artist);
        }
        if let Some(rest) = id.strip_prefix("pl-") {
            return rest.parse().ok().map(Self::Playlist);
        }
        id.parse().ok().map(Self::Song)
    }

    /// Parse an API id that must refer to a song.
    #[must_use]
    pub fn parse_song(id: &str) -> Option<i32> {
        match Self::parse(id) {
            Some(Self::Song(song_id)) => Some(song_id),
            _ => None,
        }
    }

    /// Parse an API id that must refer to an album.
    /// Bare integers are accepted as album ids (Subsonic default for
    /// album-scoped endpoints like `getAlbum`).
    #[must_use]
    pub fn parse_album(id: &str) -> Option<i32> {
        match Self::parse(id) {
            Some(Self::Album(album_id)) => Some(album_id),
            Some(Self::Song(raw)) if !id.starts_with("mf-") => Some(raw),
            _ => None,
        }
    }

    /// Parse an API id that must refer to an artist.
    /// Bare integers are accepted as artist ids.
    #[must_use]
    pub fn parse_artist(id: &str) -> Option<i32> {
        match Self::parse(id) {
            Some(Self::Artist(artist_id)) => Some(artist_id),
            Some(Self::Song(raw)) if !id.starts_with("mf-") => Some(raw),
            _ => None,
        }
    }

    /// Parse an API id that must refer to a playlist.
    /// Bare integers are accepted as playlist ids.
    #[must_use]
    pub fn parse_playlist(id: &str) -> Option<i32> {
        match Self::parse(id) {
            Some(Self::Playlist(playlist_id)) => Some(playlist_id),
            Some(Self::Song(raw)) if !id.starts_with("pl-") && !id.starts_with("mf-") => Some(raw),
            _ => None,
        }
    }
}

/// Replace Latin special letters that have no NFKD decomposition
/// (ligatures, strokes) with their ASCII equivalents.
const fn expand_latin_specials(c: char) -> &'static str {
    match c {
        'Æ' => "AE",
        'æ' => "ae",
        'Œ' => "OE",
        'œ' => "oe",
        'ß' => "ss",
        'Ø' | 'ø' => "o",
        'Ð' | 'ð' => "d",
        'Þ' | 'þ' => "th",
        'Ł' | 'ł' => "l",
        'Ħ' | 'ħ' => "h",
        'Ŋ' | 'ŋ' => "n",
        'Ŧ' | 'ŧ' => "t",
        _ => "",
    }
}

/// Fold text for search matching.
///
/// Lowercases, strips diacritics, and collapses whitespace. Stored in
/// `search_name` columns and applied to search queries so "beyonce"
/// matches "Beyoncé" (navidrome-compatible behavior).
#[must_use]
pub fn normalize_search_text(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;

    let mut expanded = String::with_capacity(text.len());
    for c in text.chars() {
        let special = expand_latin_specials(c);
        if special.is_empty() {
            expanded.push(c);
        } else {
            expanded.push_str(special);
        }
    }
    let folded: String = expanded
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize a Subsonic search query for matching: trim, strip wrapping
/// quotes, drop a trailing `*` (prefix-search marker), then fold.
#[must_use]
pub fn normalize_search_query(query: &str) -> String {
    let trimmed = query.trim().trim_matches('"').trim();
    let without_prefix_marker = trimmed.strip_suffix('*').unwrap_or(trimmed).trim_end();
    normalize_search_text(without_prefix_marker)
}

/// Format a UTC naive timestamp in Subsonic's wire format.
#[must_use]
pub fn format_subsonic_datetime(datetime: &NaiveDateTime) -> String {
    datetime.format(SUBSONIC_DATETIME_FORMAT).to_string()
}

/// Saturating cast from `i64` to `i32` for Subsonic response fields.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Subsonic count fields are signed 32-bit values"
)]
#[must_use]
pub fn saturating_i64_to_i32(value: i64) -> i32 {
    if value > i64::from(i32::MAX) {
        i32::MAX
    } else if value < i64::from(i32::MIN) {
        i32::MIN
    } else {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EntityId, album_api_id, artist_api_id, normalize_search_query, normalize_search_text,
        playlist_api_id, saturating_i64_to_i32, song_api_id,
    };

    #[test]
    fn api_id_helpers_emit_namespaced_ids() {
        assert_eq!(song_api_id(1), "mf-1");
        assert_eq!(album_api_id(1), "al-1");
        assert_eq!(artist_api_id(1), "ar-1");
        assert_eq!(playlist_api_id(1), "pl-1");
    }

    #[test]
    fn entity_id_parse_roundtrips_namespaced_ids() {
        assert_eq!(EntityId::parse("mf-42"), Some(EntityId::Song(42)));
        assert_eq!(EntityId::parse("al-42"), Some(EntityId::Album(42)));
        assert_eq!(EntityId::parse("ar-42"), Some(EntityId::Artist(42)));
        assert_eq!(EntityId::parse("pl-42"), Some(EntityId::Playlist(42)));
    }

    #[test]
    fn entity_id_parse_treats_bare_integers_as_songs() {
        assert_eq!(EntityId::parse("42"), Some(EntityId::Song(42)));
        assert_eq!(EntityId::parse(""), None);
        assert_eq!(EntityId::parse("mf-"), None);
        assert_eq!(EntityId::parse("bogus"), None);
    }

    #[test]
    fn entity_id_parse_song_accepts_mf_prefix_only() {
        assert_eq!(EntityId::parse_song("mf-42"), Some(42));
        assert_eq!(EntityId::parse_song("42"), Some(42));
        assert_eq!(EntityId::parse_song("al-42"), None);
        assert_eq!(EntityId::parse_song("ar-42"), None);
        assert_eq!(EntityId::parse_song("pl-42"), None);
    }

    #[test]
    fn entity_id_parse_album_accepts_al_prefix_and_bare_ints() {
        assert_eq!(EntityId::parse_album("al-42"), Some(42));
        assert_eq!(EntityId::parse_album("42"), Some(42));
        assert_eq!(EntityId::parse_album("mf-42"), None);
        assert_eq!(EntityId::parse_album("ar-42"), None);
        assert_eq!(EntityId::parse_album("pl-42"), None);
    }

    #[test]
    fn entity_id_parse_artist_accepts_ar_prefix_and_bare_ints() {
        assert_eq!(EntityId::parse_artist("ar-42"), Some(42));
        assert_eq!(EntityId::parse_artist("42"), Some(42));
        assert_eq!(EntityId::parse_artist("mf-42"), None);
        assert_eq!(EntityId::parse_artist("al-42"), None);
        assert_eq!(EntityId::parse_artist("pl-42"), None);
    }

    #[test]
    fn entity_id_parse_playlist_accepts_pl_prefix_and_bare_ints() {
        assert_eq!(EntityId::parse_playlist("pl-42"), Some(42));
        assert_eq!(EntityId::parse_playlist("42"), Some(42));
        assert_eq!(EntityId::parse_playlist("mf-42"), None);
        assert_eq!(EntityId::parse_playlist("al-42"), None);
        assert_eq!(EntityId::parse_playlist("ar-42"), None);
    }

    #[test]
    fn normalize_search_text_folds_case_accents_and_whitespace() {
        assert_eq!(normalize_search_text("Beyoncé"), "beyonce");
        assert_eq!(normalize_search_text("Mötley   Crüe"), "motley crue");
        assert_eq!(normalize_search_text("Sigur Rós"), "sigur ros");
        assert_eq!(normalize_search_text("AC/DC"), "ac/dc");
        assert_eq!(normalize_search_text("  spaced   out  "), "spaced out");
        assert_eq!(normalize_search_text("Œuvre"), "oeuvre");
    }

    #[test]
    fn normalize_search_query_strips_quotes_and_prefix_marker() {
        assert_eq!(normalize_search_query(" \" Miles Davis \" "), "miles davis");
        assert_eq!(normalize_search_query("beat*"), "beat");
        assert_eq!(normalize_search_query("Björk*"), "bjork");
        assert_eq!(normalize_search_query(""), "");
    }

    #[test]
    fn saturating_i64_to_i32_clamps_positive_overflow() {
        assert_eq!(saturating_i64_to_i32(i64::MAX), i32::MAX);
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MAX) + 1), i32::MAX);
    }

    #[test]
    fn saturating_i64_to_i32_clamps_negative_overflow() {
        assert_eq!(saturating_i64_to_i32(i64::MIN), i32::MIN);
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MIN) - 1), i32::MIN);
    }

    #[test]
    fn saturating_i64_to_i32_passes_through_in_range() {
        assert_eq!(saturating_i64_to_i32(0), 0);
        assert_eq!(saturating_i64_to_i32(-1), -1);
        assert_eq!(saturating_i64_to_i32(42), 42);
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MAX)), i32::MAX);
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MIN)), i32::MIN);
    }
}

/// A music folder (library root directory).
#[derive(Debug, Clone)]
pub struct MusicFolder {
    pub id: i32,
    pub name: String,
    pub path: String,
    pub parent: Option<i32>,
    pub enabled: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Subsonic API music folder response format.
#[derive(Debug, Serialize, Clone)]
pub struct MusicFolderResponse {
    #[serde(rename = "@id")]
    pub id: i32,
    #[serde(rename = "@name")]
    pub name: String,
}

impl From<&MusicFolder> for MusicFolderResponse {
    fn from(folder: &MusicFolder) -> Self {
        Self {
            id: folder.id,
            name: folder.name.clone(),
        }
    }
}

/// An artist in the music library.
#[derive(Debug, Clone)]
pub struct Artist {
    pub id: i32,
    pub name: String,
    pub sort_name: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub cover_art: Option<String>,
    pub artist_image_url: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Subsonic API artist response format (for getIndexes).
#[derive(Debug, Serialize, Clone)]
pub struct ArtistResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@artistImageUrl", skip_serializing_if = "Option::is_none")]
    pub artist_image_url: Option<String>,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@userRating", skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<i32>,
    #[serde(rename = "@averageRating", skip_serializing_if = "Option::is_none")]
    pub average_rating: Option<f64>,
}

impl From<&Artist> for ArtistResponse {
    fn from(artist: &Artist) -> Self {
        Self::from_artist_with_starred(artist, None)
    }
}

/// Subsonic API artist ID3 response format (for getArtists).
#[derive(Debug, Serialize, Clone)]
pub struct ArtistID3Response {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@artistImageUrl", skip_serializing_if = "Option::is_none")]
    pub artist_image_url: Option<String>,
    #[serde(rename = "@albumCount", skip_serializing_if = "Option::is_none")]
    pub album_count: Option<i32>,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "@sortName", skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
}

impl ArtistID3Response {
    #[must_use]
    pub fn from_artist(artist: &Artist, album_count: Option<i32>) -> Self {
        Self {
            id: artist_api_id(artist.id),
            name: artist.name.clone(),
            cover_art: artist.cover_art.clone(),
            artist_image_url: artist.artist_image_url.clone(),
            album_count,
            starred: None,
            musicbrainz_id: artist.musicbrainz_id.clone(),
            sort_name: artist.sort_name.clone(),
        }
    }

    #[must_use]
    pub fn from_artist_with_starred(
        artist: &Artist,
        album_count: Option<i32>,
        starred_at: Option<&NaiveDateTime>,
    ) -> Self {
        Self {
            id: artist_api_id(artist.id),
            name: artist.name.clone(),
            cover_art: artist.cover_art.clone(),
            artist_image_url: artist.artist_image_url.clone(),
            album_count,
            starred: starred_at.map(format_subsonic_datetime),
            musicbrainz_id: artist.musicbrainz_id.clone(),
            sort_name: artist.sort_name.clone(),
        }
    }
}

/// An album in the music library.
#[derive(Debug, Clone)]
pub struct Album {
    pub id: i32,
    pub name: String,
    pub sort_name: Option<String>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub duration: i32,
    pub song_count: i32,
    pub play_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Subsonic API album ID3 response format.
#[derive(Debug, Serialize, Clone)]
pub struct AlbumID3Response {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@artistId", skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@songCount")]
    pub song_count: i32,
    #[serde(rename = "@duration")]
    pub duration: i32,
    #[serde(rename = "@playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i32>,
    #[serde(rename = "@created")]
    pub created: String,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@year", skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(rename = "@genre", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "@userRating", skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<i32>,
    #[serde(rename = "@averageRating", skip_serializing_if = "Option::is_none")]
    pub average_rating: Option<f64>,
    #[serde(rename = "@played", skip_serializing_if = "Option::is_none")]
    pub played: Option<String>,
    #[serde(rename = "@sortName", skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
}

/// Per-user annotation data for an album (ratings, last played).
#[derive(Debug, Clone, Copy, Default)]
pub struct AlbumAnnotations {
    pub user_rating: Option<i32>,
    pub average_rating: Option<f64>,
    pub played_at: Option<NaiveDateTime>,
}

impl From<&Album> for AlbumID3Response {
    fn from(album: &Album) -> Self {
        Self::from_album_with_starred(album, None)
    }
}

impl AlbumID3Response {
    #[must_use]
    pub fn from_album_with_starred(album: &Album, starred_at: Option<&NaiveDateTime>) -> Self {
        Self {
            id: album_api_id(album.id),
            name: album.name.clone(),
            artist: album.artist_name.clone(),
            artist_id: album.artist_id.map(artist_api_id),
            cover_art: album.cover_art.clone(),
            song_count: album.song_count,
            duration: album.duration,
            play_count: Some(album.play_count),
            created: format_subsonic_datetime(&album.created_at),
            starred: starred_at.map(format_subsonic_datetime),
            year: album.year,
            genre: album.genre.clone(),
            user_rating: None,
            average_rating: None,
            played: None,
            sort_name: album.sort_name.clone(),
            musicbrainz_id: album.musicbrainz_id.clone(),
        }
    }

    /// Attach per-user annotation data (ratings, last played).
    #[must_use]
    pub fn with_annotations(mut self, annotations: Option<&AlbumAnnotations>) -> Self {
        if let Some(annotations) = annotations {
            self.user_rating = annotations.user_rating;
            self.average_rating = annotations.average_rating;
            self.played = annotations.played_at.as_ref().map(format_subsonic_datetime);
        }
        self
    }
}

/// A song/track in the music library.
#[derive(Debug, Clone)]
pub struct Song {
    pub id: i32,
    pub title: String,
    pub sort_name: Option<String>,
    pub album_id: Option<i32>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub music_folder_id: i32,
    pub path: String,
    pub cue_track_id: Option<i32>,
    pub parent_path: String,
    pub file_size: i64,
    pub content_type: String,
    pub suffix: String,
    pub duration: i32,
    pub bit_rate: Option<i32>,
    pub bit_depth: Option<i32>,
    pub sampling_rate: Option<i32>,
    pub channel_count: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub play_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Subsonic API child (song) response format.
#[derive(Debug, Serialize, Clone)]
pub struct ChildResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@parent", skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(rename = "@isDir")]
    pub is_dir: bool,
    #[serde(rename = "@title")]
    pub title: String,
    #[serde(rename = "@album", skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@track", skip_serializing_if = "Option::is_none")]
    pub track: Option<i32>,
    #[serde(rename = "@year", skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(rename = "@genre", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@size", skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(rename = "@contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(rename = "@suffix", skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(rename = "@duration", skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(rename = "@bitRate", skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<i32>,
    #[serde(rename = "@bitDepth", skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i32>,
    #[serde(rename = "@samplingRate", skip_serializing_if = "Option::is_none")]
    pub sampling_rate: Option<i32>,
    #[serde(rename = "@channelCount", skip_serializing_if = "Option::is_none")]
    pub channel_count: Option<i32>,
    #[serde(rename = "@path", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "@playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i32>,
    #[serde(rename = "@discNumber", skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<i32>,
    #[serde(rename = "@created", skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(rename = "@albumId", skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(rename = "@artistId", skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(rename = "@type", skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@userRating", skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<i32>,
    #[serde(rename = "@averageRating", skip_serializing_if = "Option::is_none")]
    pub average_rating: Option<f64>,
    #[serde(rename = "@played", skip_serializing_if = "Option::is_none")]
    pub played: Option<String>,
    #[serde(rename = "@bookmarkPosition", skip_serializing_if = "Option::is_none")]
    pub bookmark_position: Option<i64>,
    #[serde(rename = "@sortName", skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
}

/// Per-user annotation data for a song (ratings, last played, bookmark).
#[derive(Debug, Clone, Copy, Default)]
pub struct SongAnnotations {
    pub starred_at: Option<NaiveDateTime>,
    pub user_rating: Option<i32>,
    pub average_rating: Option<f64>,
    pub played_at: Option<NaiveDateTime>,
    pub bookmark_position: Option<i64>,
}

impl From<&Song> for ChildResponse {
    fn from(song: &Song) -> Self {
        Self::from_song_with_starred(song, None)
    }
}

impl ChildResponse {
    #[must_use]
    pub fn from_song_with_starred(song: &Song, starred_at: Option<&NaiveDateTime>) -> Self {
        Self {
            id: song_api_id(song.id),
            parent: song.album_id.map(album_api_id),
            is_dir: false,
            title: song.title.clone(),
            album: song.album_name.clone(),
            artist: song.artist_name.clone(),
            track: song.track_number,
            year: song.year,
            genre: song.genre.clone(),
            cover_art: song.cover_art.clone(),
            size: Some(song.file_size),
            content_type: Some(song.content_type.clone()),
            suffix: Some(song.suffix.clone()),
            duration: Some(song.duration),
            bit_rate: song.bit_rate,
            bit_depth: song.bit_depth,
            sampling_rate: song.sampling_rate,
            channel_count: song.channel_count,
            path: Some(song.path.clone()),
            play_count: Some(song.play_count),
            disc_number: song.disc_number,
            created: Some(format_subsonic_datetime(&song.created_at)),
            album_id: song.album_id.map(album_api_id),
            artist_id: song.artist_id.map(artist_api_id),
            media_type: Some("music".to_string()),
            starred: starred_at.map(format_subsonic_datetime),
            user_rating: None,
            average_rating: None,
            played: None,
            bookmark_position: None,
            sort_name: song.sort_name.clone(),
            musicbrainz_id: song.musicbrainz_id.clone(),
        }
    }

    /// Attach per-user annotation data (ratings, last played, bookmark).
    #[must_use]
    pub fn with_annotations(mut self, annotations: Option<&SongAnnotations>) -> Self {
        if let Some(annotations) = annotations {
            if let Some(starred_at) = annotations.starred_at.as_ref() {
                self.starred = Some(format_subsonic_datetime(starred_at));
            }
            self.user_rating = annotations.user_rating;
            self.average_rating = annotations.average_rating;
            self.played = annotations.played_at.as_ref().map(format_subsonic_datetime);
            self.bookmark_position = annotations.bookmark_position;
        }
        self
    }
}

/// Index entry for getIndexes response.
#[derive(Debug, Serialize, Clone)]
pub struct IndexResponse {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistResponse>,
}

/// Indexes response for getIndexes.
#[derive(Debug, Serialize, Clone)]
pub struct IndexesResponse {
    #[serde(rename = "@ignoredArticles")]
    pub ignored_articles: String,
    #[serde(rename = "@lastModified")]
    pub last_modified: i64,
    #[serde(rename = "index", skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexResponse>,
}

/// Index entry for getArtists response (ID3 version).
#[derive(Debug, Serialize, Clone)]
pub struct IndexID3Response {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistID3Response>,
}

/// Artists response for getArtists (ID3 version).
#[derive(Debug, Serialize, Clone)]
pub struct ArtistsID3Response {
    #[serde(rename = "@ignoredArticles")]
    pub ignored_articles: String,
    #[serde(rename = "@lastModified")]
    pub last_modified: i64,
    #[serde(rename = "index", skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexID3Response>,
}

/// Album with songs response for getAlbum.
#[derive(Debug, Serialize, Clone)]
pub struct AlbumWithSongsID3Response {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@artistId", skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@songCount")]
    pub song_count: i32,
    #[serde(rename = "@duration")]
    pub duration: i32,
    #[serde(rename = "@playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i32>,
    #[serde(rename = "@created")]
    pub created: String,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@year", skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(rename = "@genre", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "@userRating", skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<i32>,
    #[serde(rename = "@averageRating", skip_serializing_if = "Option::is_none")]
    pub average_rating: Option<f64>,
    #[serde(rename = "@played", skip_serializing_if = "Option::is_none")]
    pub played: Option<String>,
    #[serde(rename = "@sortName", skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

impl AlbumWithSongsID3Response {
    #[must_use]
    pub fn from_album_and_songs(album: &Album, songs: Vec<ChildResponse>) -> Self {
        Self::from_album_and_songs_with_starred(album, songs, None)
    }

    #[must_use]
    pub fn from_album_and_songs_with_starred(
        album: &Album,
        songs: Vec<ChildResponse>,
        starred_at: Option<&NaiveDateTime>,
    ) -> Self {
        Self {
            id: album_api_id(album.id),
            name: album.name.clone(),
            artist: album.artist_name.clone(),
            artist_id: album.artist_id.map(artist_api_id),
            cover_art: album.cover_art.clone(),
            song_count: album.song_count,
            duration: album.duration,
            play_count: Some(album.play_count),
            created: format_subsonic_datetime(&album.created_at),
            starred: starred_at.map(format_subsonic_datetime),
            year: album.year,
            genre: album.genre.clone(),
            user_rating: None,
            average_rating: None,
            played: None,
            sort_name: album.sort_name.clone(),
            musicbrainz_id: album.musicbrainz_id.clone(),
            songs,
        }
    }

    /// Attach per-user annotation data (ratings, last played).
    #[must_use]
    pub fn with_annotations(mut self, annotations: Option<&AlbumAnnotations>) -> Self {
        if let Some(annotations) = annotations {
            self.user_rating = annotations.user_rating;
            self.average_rating = annotations.average_rating;
            self.played = annotations.played_at.as_ref().map(format_subsonic_datetime);
        }
        self
    }
}

/// Artist with albums response for getArtist.
#[derive(Debug, Serialize, Clone)]
pub struct ArtistWithAlbumsID3Response {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@artistImageUrl", skip_serializing_if = "Option::is_none")]
    pub artist_image_url: Option<String>,
    #[serde(rename = "@albumCount", skip_serializing_if = "Option::is_none")]
    pub album_count: Option<i32>,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "@sortName", skip_serializing_if = "Option::is_none")]
    pub sort_name: Option<String>,
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<AlbumID3Response>,
}

impl ArtistWithAlbumsID3Response {
    #[must_use]
    pub fn from_artist_and_albums(artist: &Artist, albums: Vec<AlbumID3Response>) -> Self {
        Self {
            id: artist_api_id(artist.id),
            name: artist.name.clone(),
            cover_art: artist.cover_art.clone(),
            artist_image_url: artist.artist_image_url.clone(),
            album_count: Some(i32::try_from(albums.len()).unwrap_or(0)),
            starred: None,
            musicbrainz_id: artist.musicbrainz_id.clone(),
            sort_name: artist.sort_name.clone(),
            albums,
        }
    }

    #[must_use]
    pub fn from_artist_and_albums_with_starred(
        artist: &Artist,
        albums: Vec<AlbumID3Response>,
        starred_at: Option<&NaiveDateTime>,
    ) -> Self {
        Self {
            id: artist_api_id(artist.id),
            name: artist.name.clone(),
            cover_art: artist.cover_art.clone(),
            artist_image_url: artist.artist_image_url.clone(),
            album_count: Some(i32::try_from(albums.len()).unwrap_or(0)),
            starred: starred_at.map(format_subsonic_datetime),
            musicbrainz_id: artist.musicbrainz_id.clone(),
            sort_name: artist.sort_name.clone(),
            albums,
        }
    }
}

/// New music folder for insertion.
#[derive(Debug, Clone)]
pub struct NewMusicFolder {
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

impl NewMusicFolder {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            enabled: true,
        }
    }
}

/// New artist for insertion.
#[derive(Debug, Clone, Default)]
pub struct NewArtist {
    pub name: String,
    pub sort_name: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub cover_art: Option<String>,
    pub artist_image_url: Option<String>,
}

impl NewArtist {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// New album for insertion.
#[derive(Debug, Clone, Default)]
pub struct NewAlbum {
    pub name: String,
    pub sort_name: Option<String>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub duration: i32,
    pub song_count: i32,
}

impl NewAlbum {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// New song for insertion.
#[derive(Debug, Clone)]
pub struct NewSong {
    pub title: String,
    pub sort_name: Option<String>,
    pub album_id: Option<i32>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub music_folder_id: i32,
    pub path: String,
    pub cue_track_id: Option<i32>,
    pub parent_path: String,
    pub file_size: i64,
    pub content_type: String,
    pub suffix: String,
    pub duration: i32,
    pub bit_rate: Option<i32>,
    pub bit_depth: Option<i32>,
    pub sampling_rate: Option<i32>,
    pub channel_count: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
}

// ============================================================================
// Response types for getAlbumList2, getGenres, search3
// ============================================================================

/// Album list response for getAlbumList2.
#[derive(Debug, Serialize, Clone)]
pub struct AlbumList2Response {
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<AlbumID3Response>,
}

/// Genre response for getGenres.
#[derive(Debug, Serialize, Clone)]
pub struct GenreResponse {
    #[serde(rename = "@songCount")]
    pub song_count: i64,
    #[serde(rename = "@albumCount")]
    pub album_count: i64,
    #[serde(rename = "$text")]
    pub value: String,
}

/// Genres response for getGenres.
#[derive(Debug, Serialize, Clone)]
pub struct GenresResponse {
    #[serde(rename = "genre", skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<GenreResponse>,
}

/// Search result response for search3.
#[derive(Debug, Serialize, Clone)]
pub struct SearchResult3Response {
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistID3Response>,
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<AlbumID3Response>,
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

// ============================================================================
// Response types for starred (getStarred2)
// ============================================================================

/// Starred2 response for getStarred2.
#[derive(Debug, Serialize, Clone)]
pub struct Starred2Response {
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistID3Response>,
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<AlbumID3Response>,
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

// ============================================================================
// Response types for bookmarks (getBookmarks)
// ============================================================================

/// A bookmark entry: a position within a song.
#[derive(Debug, Serialize, Clone)]
pub struct BookmarkResponse {
    #[serde(rename = "@position")]
    pub position: i64,
    #[serde(rename = "@username")]
    pub username: String,
    #[serde(rename = "@comment", skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(rename = "@created")]
    pub created: String,
    #[serde(rename = "@changed")]
    pub changed: String,
    pub entry: ChildResponse,
}

/// Bookmarks response for getBookmarks.
#[derive(Debug, Serialize, Clone)]
pub struct BookmarksResponse {
    #[serde(rename = "bookmark", skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<BookmarkResponse>,
}

// ============================================================================
// Response types for internet radio stations
// ============================================================================

/// An internet radio station response.
#[derive(Debug, Serialize, Clone)]
pub struct InternetRadioStationResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@streamUrl")]
    pub stream_url: String,
    #[serde(rename = "@homePageUrl", skip_serializing_if = "Option::is_none")]
    pub home_page_url: Option<String>,
}

/// Internet radio stations response for getInternetRadioStations.
#[derive(Debug, Serialize, Clone)]
pub struct InternetRadioStationsResponse {
    #[serde(rename = "internetRadioStation", skip_serializing_if = "Vec::is_empty")]
    pub stations: Vec<InternetRadioStationResponse>,
}

// ============================================================================
// Response types for getNowPlaying
// ============================================================================

/// Now playing entry response for getNowPlaying.
#[derive(Debug, Serialize, Clone)]
pub struct NowPlayingEntryResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@parent", skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(rename = "@isDir")]
    pub is_dir: bool,
    #[serde(rename = "@title")]
    pub title: String,
    #[serde(rename = "@album", skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@track", skip_serializing_if = "Option::is_none")]
    pub track: Option<i32>,
    #[serde(rename = "@year", skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(rename = "@genre", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@size", skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(rename = "@contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(rename = "@suffix", skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(rename = "@duration", skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(rename = "@bitRate", skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<i32>,
    #[serde(rename = "@path", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "@albumId", skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(rename = "@artistId", skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(rename = "@type", skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    // Now playing specific fields
    #[serde(rename = "@username")]
    pub username: String,
    #[serde(rename = "@minutesAgo")]
    pub minutes_ago: i32,
    #[serde(rename = "@playerId", skip_serializing_if = "Option::is_none")]
    pub player_id: Option<String>,
}

impl NowPlayingEntryResponse {
    #[must_use]
    pub fn from_now_playing(
        song: &Song,
        username: String,
        minutes_ago: i32,
        player_id: Option<String>,
    ) -> Self {
        Self {
            id: song_api_id(song.id),
            parent: song.album_id.map(album_api_id),
            is_dir: false,
            title: song.title.clone(),
            album: song.album_name.clone(),
            artist: song.artist_name.clone(),
            track: song.track_number,
            year: song.year,
            genre: song.genre.clone(),
            cover_art: song.cover_art.clone(),
            size: Some(song.file_size),
            content_type: Some(song.content_type.clone()),
            suffix: Some(song.suffix.clone()),
            duration: Some(song.duration),
            bit_rate: song.bit_rate,
            path: Some(song.path.clone()),
            album_id: song.album_id.map(album_api_id),
            artist_id: song.artist_id.map(artist_api_id),
            media_type: Some("music".to_string()),
            username,
            minutes_ago,
            player_id,
        }
    }
}

/// Now playing response for getNowPlaying.
#[derive(Debug, Serialize, Clone)]
pub struct NowPlayingResponse {
    #[serde(rename = "entry", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<NowPlayingEntryResponse>,
}

// ============================================================================
// Response types for getRandomSongs and getSongsByGenre
// ============================================================================

/// Random songs response for getRandomSongs.
#[derive(Debug, Serialize, Clone)]
pub struct RandomSongsResponse {
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

/// Songs by genre response for getSongsByGenre.
#[derive(Debug, Serialize, Clone)]
pub struct SongsByGenreResponse {
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

// ============================================================================
// Response types for playlists
// ============================================================================

/// Playlist response for getPlaylists.
#[derive(Debug, Serialize, Clone)]
pub struct PlaylistResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@comment", skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(rename = "@owner")]
    pub owner: String,
    #[serde(rename = "@public")]
    pub public: bool,
    #[serde(rename = "@songCount")]
    pub song_count: i32,
    #[serde(rename = "@duration")]
    pub duration: i32,
    #[serde(rename = "@created")]
    pub created: String,
    #[serde(rename = "@changed")]
    pub changed: String,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
}

/// Playlists response for getPlaylists.
#[derive(Debug, Serialize, Clone)]
pub struct PlaylistsResponse {
    #[serde(rename = "playlist", skip_serializing_if = "Vec::is_empty")]
    pub playlists: Vec<PlaylistResponse>,
}

/// Playlist with songs response for getPlaylist.
#[derive(Debug, Serialize, Clone)]
pub struct PlaylistWithSongsResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@comment", skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(rename = "@owner")]
    pub owner: String,
    #[serde(rename = "@public")]
    pub public: bool,
    #[serde(rename = "@songCount")]
    pub song_count: i32,
    #[serde(rename = "@duration")]
    pub duration: i32,
    #[serde(rename = "@created")]
    pub created: String,
    #[serde(rename = "@changed")]
    pub changed: String,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "entry", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ChildResponse>,
}

// ============================================================================
// Response types for play queue
// ============================================================================

/// Play queue response for getPlayQueue.
#[derive(Debug, Serialize, Clone)]
pub struct PlayQueueResponse {
    #[serde(rename = "@current", skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(rename = "@position", skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(rename = "@username")]
    pub username: String,
    #[serde(rename = "@changed")]
    pub changed: String,
    #[serde(rename = "@changedBy", skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
    #[serde(rename = "entry", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ChildResponse>,
}

/// Play queue by index response for getPlayQueueByIndex (`OpenSubsonic`).
/// Uses currentIndex instead of current (song ID).
#[derive(Debug, Serialize, Clone)]
pub struct PlayQueueByIndexResponse {
    #[serde(rename = "@currentIndex", skip_serializing_if = "Option::is_none")]
    pub current_index: Option<i32>,
    #[serde(rename = "@position", skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(rename = "@username")]
    pub username: String,
    #[serde(rename = "@changed")]
    pub changed: String,
    #[serde(rename = "@changedBy", skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
    #[serde(rename = "entry", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ChildResponse>,
}

/// Token info response for tokenInfo (`OpenSubsonic`).
/// Returns information about the API key used for authentication.
#[derive(Debug, Serialize, Clone)]
pub struct TokenInfoResponse {
    #[serde(rename = "@username")]
    pub username: String,
}

// ============================================================================
// Response types for getArtistInfo2, getAlbumInfo2, getSimilarSongs2, getTopSongs
// ============================================================================

/// Artist info response for getArtistInfo2.
#[derive(Debug, Serialize, Clone)]
pub struct ArtistInfo2Response {
    #[serde(rename = "@biography", skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "@lastFmUrl", skip_serializing_if = "Option::is_none")]
    pub last_fm_url: Option<String>,
    #[serde(rename = "@smallImageUrl", skip_serializing_if = "Option::is_none")]
    pub small_image_url: Option<String>,
    #[serde(rename = "@mediumImageUrl", skip_serializing_if = "Option::is_none")]
    pub medium_image_url: Option<String>,
    #[serde(rename = "@largeImageUrl", skip_serializing_if = "Option::is_none")]
    pub large_image_url: Option<String>,
    #[serde(rename = "similarArtist", skip_serializing_if = "Vec::is_empty")]
    pub similar_artists: Vec<ArtistID3Response>,
}

impl ArtistInfo2Response {
    /// Create an empty artist info response (stub).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            biography: None,
            musicbrainz_id: None,
            last_fm_url: None,
            small_image_url: None,
            medium_image_url: None,
            large_image_url: None,
            similar_artists: Vec::new(),
        }
    }

    /// Create an artist info response with `musicbrainz_id` from the artist.
    #[must_use]
    pub fn from_artist(artist: &Artist) -> Self {
        Self {
            biography: None,
            musicbrainz_id: artist.musicbrainz_id.clone(),
            last_fm_url: None,
            small_image_url: artist.artist_image_url.clone(),
            medium_image_url: artist.artist_image_url.clone(),
            large_image_url: artist.artist_image_url.clone(),
            similar_artists: Vec::new(),
        }
    }
}

/// Album info response for getAlbumInfo2.
#[derive(Debug, Serialize, Clone)]
pub struct AlbumInfoResponse {
    #[serde(rename = "@notes", skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "@lastFmUrl", skip_serializing_if = "Option::is_none")]
    pub last_fm_url: Option<String>,
    #[serde(rename = "@smallImageUrl", skip_serializing_if = "Option::is_none")]
    pub small_image_url: Option<String>,
    #[serde(rename = "@mediumImageUrl", skip_serializing_if = "Option::is_none")]
    pub medium_image_url: Option<String>,
    #[serde(rename = "@largeImageUrl", skip_serializing_if = "Option::is_none")]
    pub large_image_url: Option<String>,
}

impl AlbumInfoResponse {
    /// Create an empty album info response (stub).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            notes: None,
            musicbrainz_id: None,
            last_fm_url: None,
            small_image_url: None,
            medium_image_url: None,
            large_image_url: None,
        }
    }

    /// Create an album info response with data from the album.
    #[must_use]
    pub fn from_album(album: &Album) -> Self {
        Self {
            notes: None,
            musicbrainz_id: album.musicbrainz_id.clone(),
            last_fm_url: None,
            small_image_url: None,
            medium_image_url: None,
            large_image_url: None,
        }
    }
}

/// Similar songs response for getSimilarSongs2.
#[derive(Debug, Serialize, Clone)]
pub struct SimilarSongs2Response {
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

/// Top songs response for getTopSongs.
#[derive(Debug, Serialize, Clone)]
pub struct TopSongsResponse {
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

/// Lyrics response for getLyrics (original Subsonic API).
#[derive(Debug, Serialize, Clone)]
pub struct LyricsResponse {
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "$text", skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl LyricsResponse {
    /// Create an empty lyrics response.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            artist: None,
            title: None,
            value: None,
        }
    }

    /// Create a lyrics response with data.
    #[must_use]
    pub const fn new(
        artist: Option<String>,
        title: Option<String>,
        lyrics: Option<String>,
    ) -> Self {
        Self {
            artist,
            title,
            value: lyrics,
        }
    }
}

// ============================================================================
// OpenSubsonic Structured Lyrics Types (for getLyricsBySongId)
// ============================================================================

/// A single line of lyrics, optionally with a start time for synchronized lyrics.
#[derive(Debug, Serialize, Clone)]
pub struct LyricLine {
    /// Start time in milliseconds from track start. Omit if unsynced.
    #[serde(rename = "@start", skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// The actual text of the lyric line.
    #[serde(rename = "$text")]
    pub value: String,
}

impl LyricLine {
    /// Create a new synced lyric line with a start time.
    pub fn synced(start: i64, value: impl Into<String>) -> Self {
        Self {
            start: Some(start),
            value: value.into(),
        }
    }

    /// Create a new unsynced lyric line without a start time.
    pub fn unsynced(value: impl Into<String>) -> Self {
        Self {
            start: None,
            value: value.into(),
        }
    }
}

/// Structured lyrics for a song, supporting synchronized lyrics and multiple languages.
#[derive(Debug, Serialize, Clone)]
pub struct StructuredLyrics {
    /// The artist name to display. May differ from the song's artist (e.g., localized).
    #[serde(rename = "@displayArtist", skip_serializing_if = "Option::is_none")]
    pub display_artist: Option<String>,
    /// The song title to display. May differ from the song's title (e.g., localized).
    #[serde(rename = "@displayTitle", skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    /// ISO 639 language code (e.g., "eng", "jpn"). Use "und" or "xxx" for unknown.
    #[serde(rename = "@lang")]
    pub lang: String,
    /// Offset in milliseconds. Positive = lyrics appear sooner, negative = later.
    #[serde(rename = "@offset", skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    /// Whether the lyrics are synchronized (have timestamps).
    #[serde(rename = "@synced")]
    pub synced: bool,
    /// The lyric lines, ordered by start time (synced) or appearance (unsynced).
    #[serde(rename = "line")]
    pub lines: Vec<LyricLine>,
}

impl StructuredLyrics {
    /// Create new unsynced lyrics.
    pub fn unsynced(lang: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            display_artist: None,
            display_title: None,
            lang: lang.into(),
            offset: None,
            synced: false,
            lines: lines.into_iter().map(LyricLine::unsynced).collect(),
        }
    }

    /// Create new synced lyrics.
    pub fn synced(lang: impl Into<String>, lines: Vec<(i64, String)>) -> Self {
        Self {
            display_artist: None,
            display_title: None,
            lang: lang.into(),
            offset: None,
            synced: true,
            lines: lines
                .into_iter()
                .map(|(start, value)| LyricLine::synced(start, value))
                .collect(),
        }
    }

    /// Set the display artist.
    #[must_use]
    pub fn with_display_artist(mut self, artist: impl Into<String>) -> Self {
        self.display_artist = Some(artist.into());
        self
    }

    /// Set the display title.
    #[must_use]
    pub fn with_display_title(mut self, title: impl Into<String>) -> Self {
        self.display_title = Some(title.into());
        self
    }

    /// Set the offset.
    #[must_use]
    pub const fn with_offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// Lyrics list response for getLyricsBySongId (`OpenSubsonic` extension).
#[derive(Debug, Serialize, Clone)]
pub struct LyricsListResponse {
    /// Array of structured lyrics. May have multiple entries for different languages.
    #[serde(rename = "structuredLyrics", skip_serializing_if = "Vec::is_empty")]
    pub structured_lyrics: Vec<StructuredLyrics>,
}

impl LyricsListResponse {
    /// Create an empty lyrics list response.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            structured_lyrics: Vec::new(),
        }
    }

    /// Create a lyrics list response with structured lyrics.
    #[must_use]
    pub const fn new(structured_lyrics: Vec<StructuredLyrics>) -> Self {
        Self { structured_lyrics }
    }
}

#[cfg(test)]
mod lyric_tests {
    use super::{LyricLine, LyricsListResponse, StructuredLyrics};

    #[test]
    fn lyric_line_constructors_preserve_sync_state() {
        let synced = LyricLine::synced(1234, "line");
        assert_eq!(synced.start, Some(1234));
        assert_eq!(synced.value, "line");

        let unsynced = LyricLine::unsynced("plain");
        assert_eq!(unsynced.start, None);
        assert_eq!(unsynced.value, "plain");
    }

    #[test]
    fn structured_lyrics_builders_preserve_lang_sync_lines_and_metadata() {
        let lyrics = StructuredLyrics::synced("eng", vec![(500, "hello".to_string())])
            .with_display_artist("Display Artist")
            .with_display_title("Display Title")
            .with_offset(-120);

        assert!(lyrics.synced);
        assert_eq!(lyrics.lang, "eng");
        assert_eq!(lyrics.display_artist.as_deref(), Some("Display Artist"));
        assert_eq!(lyrics.display_title.as_deref(), Some("Display Title"));
        assert_eq!(lyrics.offset, Some(-120));
        assert_eq!(lyrics.lines[0].start, Some(500));
        assert_eq!(lyrics.lines[0].value, "hello");
    }

    #[test]
    fn unsynced_structured_lyrics_omit_line_start_times() {
        let lyrics = StructuredLyrics::unsynced("und", vec!["a".to_string(), "b".to_string()]);

        assert!(!lyrics.synced);
        assert_eq!(lyrics.lang, "und");
        assert_eq!(lyrics.lines.len(), 2);
        assert!(lyrics.lines.iter().all(|line| line.start.is_none()));
    }

    #[test]
    fn lyrics_list_response_preserves_empty_and_non_empty_payloads() {
        assert!(LyricsListResponse::empty().structured_lyrics.is_empty());

        let lyrics = StructuredLyrics::unsynced("eng", vec!["line".to_string()]);
        let response = LyricsListResponse::new(vec![lyrics]);
        assert_eq!(response.structured_lyrics.len(), 1);
    }
}

// ============================================================================
// Response types for getMusicDirectory (non-ID3 folder browsing)
// ============================================================================

/// Directory response for getMusicDirectory.
#[derive(Debug, Serialize, Clone)]
pub struct DirectoryResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@parent", skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@starred", skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(rename = "@playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i32>,
    #[serde(rename = "child", skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildResponse>,
}

impl DirectoryResponse {
    /// Create a directory response from a music folder.
    #[must_use]
    pub fn from_music_folder(folder: &MusicFolder, children: Vec<ChildResponse>) -> Self {
        Self {
            id: folder.id.to_string(),
            parent: None,
            name: folder.name.clone(),
            starred: None,
            play_count: None,
            children,
        }
    }

    /// Create a directory response from an artist.
    #[must_use]
    pub fn from_artist(artist: &Artist, children: Vec<ChildResponse>) -> Self {
        Self {
            id: artist_api_id(artist.id),
            parent: None,
            name: artist.name.clone(),
            starred: None,
            play_count: None,
            children,
        }
    }

    /// Create a directory response from an album.
    #[must_use]
    pub fn from_album(album: &Album, children: Vec<ChildResponse>) -> Self {
        Self {
            id: album_api_id(album.id),
            parent: album.artist_id.map(artist_api_id),
            name: album.name.clone(),
            starred: None,
            play_count: Some(album.play_count),
            children,
        }
    }
}

impl ChildResponse {
    /// Create a child response representing an artist (as directory).
    #[must_use]
    pub fn from_artist_as_dir(artist: &Artist) -> Self {
        Self {
            id: artist_api_id(artist.id),
            parent: None,
            is_dir: true,
            title: artist.name.clone(),
            album: None,
            artist: Some(artist.name.clone()),
            track: None,
            year: None,
            genre: None,
            cover_art: artist.cover_art.clone(),
            size: None,
            content_type: None,
            suffix: None,
            duration: None,
            bit_rate: None,
            bit_depth: None,
            sampling_rate: None,
            channel_count: None,
            path: None,
            play_count: None,
            disc_number: None,
            created: Some(format_subsonic_datetime(&artist.created_at)),
            album_id: None,
            artist_id: Some(artist_api_id(artist.id)),
            media_type: None,
            starred: None,
            user_rating: None,
            average_rating: None,
            played: None,
            bookmark_position: None,
            sort_name: artist.sort_name.clone(),
            musicbrainz_id: artist.musicbrainz_id.clone(),
        }
    }

    /// Create a child response representing an album (as directory).
    #[must_use]
    pub fn from_album_as_dir(album: &Album) -> Self {
        Self {
            id: album_api_id(album.id),
            parent: album.artist_id.map(artist_api_id),
            is_dir: true,
            title: album.name.clone(),
            album: Some(album.name.clone()),
            artist: album.artist_name.clone(),
            track: None,
            year: album.year,
            genre: album.genre.clone(),
            cover_art: album.cover_art.clone(),
            size: None,
            content_type: None,
            suffix: None,
            duration: Some(album.duration),
            bit_rate: None,
            bit_depth: None,
            sampling_rate: None,
            channel_count: None,
            path: None,
            play_count: Some(album.play_count),
            disc_number: None,
            created: Some(format_subsonic_datetime(&album.created_at)),
            album_id: Some(album_api_id(album.id)),
            artist_id: album.artist_id.map(artist_api_id),
            media_type: None,
            starred: None,
            user_rating: None,
            average_rating: None,
            played: None,
            bookmark_position: None,
            sort_name: album.sort_name.clone(),
            musicbrainz_id: album.musicbrainz_id.clone(),
        }
    }
}

#[cfg(test)]
mod directory_tests {
    use chrono::{NaiveDate, NaiveDateTime};

    use super::{Album, Artist, ChildResponse, DirectoryResponse, MusicFolder};

    fn ts() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 1, 2)
            .expect("valid date")
            .and_hms_milli_opt(3, 4, 5, 678)
            .expect("valid time")
    }

    fn artist() -> Artist {
        Artist {
            id: 11,
            name: "Artist".to_string(),
            sort_name: Some("Artist Sort".to_string()),
            musicbrainz_id: Some("mb-artist".to_string()),
            cover_art: Some("artist-cover".to_string()),
            artist_image_url: Some("https://example.test/artist.jpg".to_string()),
            created_at: ts(),
            updated_at: ts(),
        }
    }

    fn album() -> Album {
        Album {
            id: 22,
            name: "Album".to_string(),
            sort_name: None,
            artist_id: Some(11),
            artist_name: Some("Artist".to_string()),
            year: Some(2024),
            genre: Some("Jazz".to_string()),
            cover_art: Some("album-cover".to_string()),
            musicbrainz_id: Some("mb-album".to_string()),
            duration: 123,
            song_count: 8,
            play_count: 5,
            created_at: ts(),
            updated_at: ts(),
        }
    }

    #[test]
    fn directory_response_from_music_folder_uses_folder_as_root() {
        let folder = MusicFolder {
            parent: None,
            id: 3,
            name: "Library".to_string(),
            path: "/music".to_string(),
            enabled: true,
            created_at: ts(),
            updated_at: ts(),
        };
        let child = ChildResponse::from_artist_as_dir(&artist());

        let response = DirectoryResponse::from_music_folder(&folder, vec![child]);

        assert_eq!(response.id, "3");
        assert_eq!(response.parent, None);
        assert_eq!(response.name, "Library");
        assert_eq!(response.children.len(), 1);
    }

    #[test]
    fn directory_response_from_album_uses_artist_parent_and_play_count() {
        let response = DirectoryResponse::from_album(&album(), Vec::new());

        assert_eq!(response.id, "al-22");
        assert_eq!(response.parent.as_deref(), Some("ar-11"));
        assert_eq!(response.name, "Album");
        assert_eq!(response.play_count, Some(5));
    }

    #[test]
    fn child_response_from_artist_as_dir_pins_directory_fields() {
        let response = ChildResponse::from_artist_as_dir(&artist());

        assert_eq!(response.id, "ar-11");
        assert!(response.is_dir);
        assert_eq!(response.title, "Artist");
        assert_eq!(response.artist.as_deref(), Some("Artist"));
        assert_eq!(response.artist_id.as_deref(), Some("ar-11"));
        assert_eq!(response.cover_art.as_deref(), Some("artist-cover"));
        assert_eq!(
            response.created.as_deref(),
            Some("2024-01-02T03:04:05.678Z")
        );
        assert_eq!(response.album_id, None);
        assert_eq!(response.duration, None);
    }

    #[test]
    fn child_response_from_album_as_dir_pins_album_metadata() {
        let response = ChildResponse::from_album_as_dir(&album());

        assert_eq!(response.id, "al-22");
        assert_eq!(response.parent.as_deref(), Some("ar-11"));
        assert!(response.is_dir);
        assert_eq!(response.title, "Album");
        assert_eq!(response.album.as_deref(), Some("Album"));
        assert_eq!(response.artist.as_deref(), Some("Artist"));
        assert_eq!(response.year, Some(2024));
        assert_eq!(response.genre.as_deref(), Some("Jazz"));
        assert_eq!(response.duration, Some(123));
        assert_eq!(response.play_count, Some(5));
        assert_eq!(response.album_id.as_deref(), Some("al-22"));
        assert_eq!(response.artist_id.as_deref(), Some("ar-11"));
    }
}

// ============================================================================
// Response types for getAlbumList (non-ID3)
// ============================================================================

/// Album list response for getAlbumList (non-ID3 version).
#[derive(Debug, Serialize, Clone)]
pub struct AlbumListResponse {
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<ChildResponse>,
}

// ============================================================================
// Response types for getStarred (non-ID3)
// ============================================================================

/// Starred response for getStarred (non-ID3).
#[derive(Debug, Serialize, Clone)]
pub struct StarredResponse {
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistResponse>,
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<ChildResponse>,
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

impl ArtistResponse {
    /// Create an artist response with starred timestamp.
    #[must_use]
    pub fn from_artist_with_starred(
        artist: &Artist,
        starred_at: Option<&chrono::NaiveDateTime>,
    ) -> Self {
        Self {
            id: artist_api_id(artist.id),
            name: artist.name.clone(),
            artist_image_url: artist.artist_image_url.clone(),
            starred: starred_at.map(format_subsonic_datetime),
            user_rating: None,
            average_rating: None,
        }
    }
}

// ============================================================================
// Response types for search2 (older search API)
// ============================================================================

/// Search result response for search2 (non-ID3).
#[derive(Debug, Serialize, Clone)]
pub struct SearchResult2Response {
    #[serde(rename = "artist", skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistResponse>,
    #[serde(rename = "album", skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<ChildResponse>,
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

// ============================================================================
// Response types for search (legacy search API)
// ============================================================================

/// Match entry for legacy search.
#[derive(Debug, Serialize, Clone)]
pub struct SearchMatch {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@parent", skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(rename = "@isDir")]
    pub is_dir: bool,
    #[serde(rename = "@title")]
    pub title: String,
    #[serde(rename = "@album", skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(rename = "@artist", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "@track", skip_serializing_if = "Option::is_none")]
    pub track: Option<i32>,
    #[serde(rename = "@year", skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(rename = "@genre", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "@coverArt", skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(rename = "@size", skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(rename = "@contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(rename = "@suffix", skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(rename = "@duration", skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(rename = "@bitRate", skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<i32>,
    #[serde(rename = "@path", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "@created", skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

impl From<&Song> for SearchMatch {
    fn from(song: &Song) -> Self {
        Self {
            id: song_api_id(song.id),
            parent: song.album_id.map(album_api_id),
            is_dir: false,
            title: song.title.clone(),
            album: song.album_name.clone(),
            artist: song.artist_name.clone(),
            track: song.track_number,
            year: song.year,
            genre: song.genre.clone(),
            cover_art: song.cover_art.clone(),
            size: Some(song.file_size),
            content_type: Some(song.content_type.clone()),
            suffix: Some(song.suffix.clone()),
            duration: Some(song.duration),
            bit_rate: song.bit_rate,
            path: Some(song.path.clone()),
            created: Some(format_subsonic_datetime(&song.created_at)),
        }
    }
}

/// Search result response for legacy search.
#[derive(Debug, Serialize, Clone)]
pub struct SearchResultResponse {
    #[serde(rename = "@offset")]
    pub offset: i64,
    #[serde(rename = "@totalHits")]
    pub total_hits: i64,
    #[serde(rename = "match", skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<SearchMatch>,
}

// ============================================================================
// Response types for getArtistInfo (non-ID3)
// ============================================================================

/// Artist info response for getArtistInfo (non-ID3).
#[derive(Debug, Serialize, Clone)]
pub struct ArtistInfoResponse {
    #[serde(rename = "@biography", skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    #[serde(rename = "@musicBrainzId", skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    #[serde(rename = "@lastFmUrl", skip_serializing_if = "Option::is_none")]
    pub last_fm_url: Option<String>,
    #[serde(rename = "@smallImageUrl", skip_serializing_if = "Option::is_none")]
    pub small_image_url: Option<String>,
    #[serde(rename = "@mediumImageUrl", skip_serializing_if = "Option::is_none")]
    pub medium_image_url: Option<String>,
    #[serde(rename = "@largeImageUrl", skip_serializing_if = "Option::is_none")]
    pub large_image_url: Option<String>,
    #[serde(rename = "similarArtist", skip_serializing_if = "Vec::is_empty")]
    pub similar_artists: Vec<ArtistResponse>,
}

impl ArtistInfoResponse {
    /// Create an artist info response from an artist.
    #[must_use]
    pub fn from_artist(artist: &Artist) -> Self {
        Self {
            biography: None,
            musicbrainz_id: artist.musicbrainz_id.clone(),
            last_fm_url: None,
            small_image_url: artist.artist_image_url.clone(),
            medium_image_url: artist.artist_image_url.clone(),
            large_image_url: artist.artist_image_url.clone(),
            similar_artists: Vec::new(),
        }
    }
}

// ============================================================================
// Response types for getSimilarSongs (non-ID3)
// ============================================================================

/// Similar songs response for getSimilarSongs (non-ID3).
#[derive(Debug, Serialize, Clone)]
pub struct SimilarSongsResponse {
    #[serde(rename = "song", skip_serializing_if = "Vec::is_empty")]
    pub songs: Vec<ChildResponse>,
}

// ============================================================================
// Response types for remote control extension
// ============================================================================

/// Remote session payload.
#[derive(Debug, Serialize, Clone)]
pub struct RemoteSessionResponse {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@pairingCode", skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
    #[serde(rename = "@expiresAt")]
    pub expires_at: String,
    #[serde(rename = "@hostDeviceId")]
    pub host_device_id: String,
    #[serde(rename = "@hostDeviceName", skip_serializing_if = "Option::is_none")]
    pub host_device_name: Option<String>,
    #[serde(
        rename = "@controllerDeviceId",
        skip_serializing_if = "Option::is_none"
    )]
    pub controller_device_id: Option<String>,
    #[serde(
        rename = "@controllerDeviceName",
        skip_serializing_if = "Option::is_none"
    )]
    pub controller_device_name: Option<String>,
    #[serde(rename = "@connected")]
    pub connected: bool,
}

/// Remote command payload.
#[derive(Debug, Serialize, Clone)]
pub struct RemoteCommandResponse {
    #[serde(rename = "@id")]
    pub id: i64,
    #[serde(rename = "@command")]
    pub command: String,
    #[serde(rename = "@payload", skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    #[serde(rename = "@sourceDeviceId")]
    pub source_device_id: String,
    #[serde(rename = "@created")]
    pub created: String,
}

/// Response payload containing queued remote commands.
#[derive(Debug, Serialize, Clone)]
pub struct RemoteCommandsResponse {
    #[serde(rename = "command", skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<RemoteCommandResponse>,
}

/// Latest remote playback state payload.
#[derive(Debug, Serialize, Clone)]
pub struct RemoteStateResponse {
    #[serde(rename = "@stateJson")]
    pub state_json: String,
    #[serde(rename = "@updatedByDeviceId")]
    pub updated_by_device_id: String,
    #[serde(rename = "@updatedAt")]
    pub updated_at: String,
}
