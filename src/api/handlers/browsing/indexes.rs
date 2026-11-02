//! Index and structure browsing handlers.

use std::collections::BTreeMap;

use axum::response::IntoResponse;
use serde::Deserialize;

use crate::api::auth::SubsonicContext;
use crate::api::handlers::util;
use crate::api::response::SubsonicResponse;
use crate::models::music::{
    Artist, ArtistID3Response, ArtistResponse, ArtistsID3Response, IndexID3Response, IndexResponse,
    IndexesResponse, MusicFolderResponse, saturating_i64_to_i32,
};

/// Query parameters for getIndexes/getArtists.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct IndexesParams {
    /// Only return data if the library changed since this time (epoch millis).
    pub if_modified_since: Option<i64>,
    pub music_folder_id: Option<i32>,
}

/// Resolve the library lastModified time in epoch millis: the latest of the
/// last completed scan and the last artist update.
fn library_last_modified(auth: &SubsonicContext) -> Result<i64, crate::db::MusicRepoError> {
    let scan_ms = auth.music().get_last_scan_at_ms()?;
    let artists_ms = auth
        .music()
        .get_artists_last_modified()?
        .map_or(0, |dt| dt.and_utc().timestamp_millis());
    Ok(scan_ms.unwrap_or(0).max(artists_ms))
}

fn artist_index_key(artist: &Artist) -> String {
    let first_char = artist
        .sort_name
        .as_ref()
        .unwrap_or(&artist.name)
        .chars()
        .next()
        .unwrap_or('#')
        .to_uppercase()
        .next()
        .unwrap_or('#');

    if first_char.is_alphabetic() {
        first_char.to_string()
    } else {
        "#".to_string()
    }
}

/// GET/POST /rest/getMusicFolders[.view]
///
/// Returns all configured top-level music folders.
pub async fn get_music_folders(auth: SubsonicContext) -> impl IntoResponse {
    let folders = match auth.music().get_music_folders() {
        Ok(folders) => folders,
        Err(e) => {
            return util::repo_error(&auth, e);
        }
    };
    let responses: Vec<MusicFolderResponse> =
        folders.iter().map(MusicFolderResponse::from).collect();
    SubsonicResponse::music_folders(auth.format, responses).into_response()
}

/// GET/POST /rest/getIndexes[.view]
///
/// Returns an indexed structure of all artists.
/// This is used by older clients that use the folder-based browsing model.
/// When `ifModifiedSince` is given and the library hasn't changed since,
/// returns an empty index list with the current `lastModified` time.
pub async fn get_indexes(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<IndexesParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let if_modified_since = params.if_modified_since;
    let blocking_auth = auth.clone();
    let response = match util::run_blocking(
        move || -> Result<IndexesResponse, crate::db::MusicRepoError> {
            let last_modified = library_last_modified(&blocking_auth)?;

            // Not modified since the client's cached version: return empty indexes
            if last_modified > 0 && if_modified_since.unwrap_or(0) >= last_modified {
                return Ok(IndexesResponse {
                    ignored_articles: "The El La Los Las Le Les".to_string(),
                    last_modified,
                    indexes: Vec::new(),
                });
            }

            let artists = blocking_auth.music().get_artists(params.music_folder_id)?;
            let user_id = blocking_auth.user.id;

            // Get starred status for all artists in a single batch query
            let artist_ids: Vec<i32> = artists.iter().map(|a| a.id).collect();
            let starred_map = blocking_auth
                .music()
                .get_starred_at_for_artists_batch(user_id, &artist_ids)?;

            // Group artists by first letter
            let mut index_map: BTreeMap<String, Vec<ArtistResponse>> = BTreeMap::new();

            for artist in &artists {
                let starred_at = starred_map.get(&artist.id);

                index_map
                    .entry(artist_index_key(artist))
                    .or_default()
                    .push(ArtistResponse::from_artist_with_starred(artist, starred_at));
            }

            // Convert to response format
            let indexes: Vec<IndexResponse> = index_map
                .into_iter()
                .map(|(name, artists)| IndexResponse { name, artists })
                .collect();

            Ok(IndexesResponse {
                ignored_articles: "The El La Los Las Le Les".to_string(),
                last_modified,
                indexes,
            })
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return util::api_error(&auth, &error),
    };

    SubsonicResponse::indexes(auth.format, response).into_response()
}

/// GET/POST /rest/getArtists[.view]
///
/// Similar to getIndexes, but returns artists using ID3 tags.
/// This is the preferred endpoint for modern clients.
/// Unlike getIndexes, `ifModifiedSince` is ignored (navidrome-compatible).
pub async fn get_artists(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<IndexesParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let _ = params.if_modified_since;
    let blocking_auth = auth.clone();
    let response = match util::run_blocking(
        move || -> Result<ArtistsID3Response, crate::db::MusicRepoError> {
            let last_modified = library_last_modified(&blocking_auth)?;

            let artists = blocking_auth.music().get_artists(params.music_folder_id)?;
            let user_id = blocking_auth.user.id;

            // Get album counts for all artists in a single batch query
            let artist_ids: Vec<i32> = artists.iter().map(|a| a.id).collect();
            let album_counts = blocking_auth
                .music()
                .get_artist_album_counts_batch(&artist_ids)?;

            // Get starred status for all artists in a single batch query
            let starred_map = blocking_auth
                .music()
                .get_starred_at_for_artists_batch(user_id, &artist_ids)?;

            // Group artists by first letter
            let mut index_map: BTreeMap<String, Vec<ArtistID3Response>> = BTreeMap::new();

            for artist in &artists {
                // Get album count and starred status from batch results
                let album_count = album_counts.get(&artist.id).copied().unwrap_or(0);
                let starred_at = starred_map.get(&artist.id);

                index_map.entry(artist_index_key(artist)).or_default().push(
                    ArtistID3Response::from_artist_with_starred(
                        artist,
                        Some(saturating_i64_to_i32(album_count)),
                        starred_at,
                    ),
                );
            }

            // Convert to response format
            let indexes: Vec<IndexID3Response> = index_map
                .into_iter()
                .map(|(name, artists)| IndexID3Response { name, artists })
                .collect();

            Ok(ArtistsID3Response {
                ignored_articles: "The El La Los Las Le Les".to_string(),
                last_modified,
                indexes,
            })
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return util::api_error(&auth, &error),
    };

    SubsonicResponse::artists(auth.format, response).into_response()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::artist_index_key;
    use crate::models::music::Artist;

    fn artist(name: &str, sort_name: Option<&str>) -> Artist {
        let now = NaiveDate::from_ymd_opt(2024, 1, 2)
            .expect("valid date")
            .and_hms_opt(3, 4, 5)
            .expect("valid time");

        Artist {
            id: 1,
            name: name.to_string(),
            sort_name: sort_name.map(str::to_string),
            musicbrainz_id: None,
            cover_art: None,
            artist_image_url: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn artist_index_key_prefers_sort_name_and_uppercases_first_letter() {
        assert_eq!(
            artist_index_key(&artist("The Beatles", Some("beatles"))),
            "B"
        );
    }

    #[test]
    fn artist_index_key_groups_non_alphabetic_and_empty_names_under_hash() {
        assert_eq!(artist_index_key(&artist("123 Go", None)), "#");
        assert_eq!(artist_index_key(&artist("", None)), "#");
    }
}
