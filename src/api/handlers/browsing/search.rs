//! Search handlers.

use std::collections::HashMap;

use axum::response::IntoResponse;
use chrono::NaiveDateTime;
use serde::Deserialize;

use crate::api::auth::SubsonicContext;
use crate::api::error::ApiError;
use crate::api::handlers::util;
use crate::api::response::SubsonicResponse;
use crate::models::music::{
    ArtistID3Response, ArtistResponse, ChildResponse, SearchMatch, SearchResult2Response,
    SearchResult3Response, SearchResultResponse, format_subsonic_datetime, saturating_i64_to_i32,
};

struct SearchLimits {
    query: String,
    artist_count: i64,
    artist_offset: i64,
    album_count: i64,
    album_offset: i64,
    song_count: i64,
    song_offset: i64,
    music_folder_id: Option<i32>,
}

struct SearchData {
    artists: Vec<crate::models::music::Artist>,
    albums: Vec<crate::models::music::Album>,
    songs: Vec<crate::models::music::Song>,
}

fn search_artist_stars(
    auth: &SubsonicContext,
    data: &SearchData,
) -> Result<HashMap<i32, NaiveDateTime>, ApiError> {
    let artist_ids: Vec<i32> = data.artists.iter().map(|artist| artist.id).collect();
    auth.music()
        .get_starred_at_for_artists_batch(auth.user.id, &artist_ids)
        .map_err(ApiError::from)
}

fn search_limits(params: &SearchParamsV2) -> SearchLimits {
    SearchLimits {
        query: crate::models::music::normalize_search_query(params.query.as_deref().unwrap_or("")),
        artist_count: params.artist_count.unwrap_or(20).clamp(0, 500),
        artist_offset: params.artist_offset.unwrap_or(0).max(0),
        album_count: params.album_count.unwrap_or(20).clamp(0, 500),
        album_offset: params.album_offset.unwrap_or(0).max(0),
        song_count: params.song_count.unwrap_or(20).clamp(0, 500),
        song_offset: params.song_offset.unwrap_or(0).max(0),
        music_folder_id: params.music_folder_id,
    }
}

fn search_data(auth: &SubsonicContext, limits: &SearchLimits) -> Result<SearchData, ApiError> {
    let artists = auth
        .music()
        .search_artists(
            &limits.query,
            limits.music_folder_id,
            limits.artist_offset,
            limits.artist_count,
        )
        .map_err(ApiError::from)?;
    let albums = auth
        .music()
        .search_albums(
            &limits.query,
            limits.music_folder_id,
            limits.album_offset,
            limits.album_count,
        )
        .map_err(ApiError::from)?;
    let songs = auth
        .music()
        .search_songs(
            &limits.query,
            limits.music_folder_id,
            limits.song_offset,
            limits.song_count,
        )
        .map_err(ApiError::from)?;

    Ok(SearchData {
        artists,
        albums,
        songs,
    })
}

fn search_album_stars(
    auth: &SubsonicContext,
    data: &SearchData,
) -> Result<HashMap<i32, NaiveDateTime>, ApiError> {
    let album_ids: Vec<i32> = data.albums.iter().map(|album| album.id).collect();
    auth.music()
        .get_starred_at_for_albums_batch(auth.user.id, &album_ids)
        .map_err(ApiError::from)
}

/// Query parameters for search3/search2.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchParamsV2 {
    /// Search query.
    pub query: Option<String>,
    /// Maximum number of artists to return. Default 20.
    #[serde(rename = "artistCount")]
    pub artist_count: Option<i64>,
    /// Artist search result offset. Default 0.
    #[serde(rename = "artistOffset")]
    pub artist_offset: Option<i64>,
    /// Maximum number of albums to return. Default 20.
    #[serde(rename = "albumCount")]
    pub album_count: Option<i64>,
    /// Album search result offset. Default 0.
    #[serde(rename = "albumOffset")]
    pub album_offset: Option<i64>,
    /// Maximum number of songs to return. Default 20.
    #[serde(rename = "songCount")]
    pub song_count: Option<i64>,
    /// Song search result offset. Default 0.
    #[serde(rename = "songOffset")]
    pub song_offset: Option<i64>,
    /// Only return results from this music folder.
    #[serde(rename = "musicFolderId")]
    pub music_folder_id: Option<i32>,
}

/// GET/POST /rest/search3[.view]
///
/// Returns albums, artists and songs matching the given search criteria.
/// Supports paging through the result.
/// An empty query returns all results (up to the count limits).
pub async fn search3(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SearchParamsV2>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let limits = search_limits(&params);
    let data = match search_data(&auth, &limits) {
        Ok(data) => data,
        Err(error) => return util::api_error(&auth, &error),
    };

    let artist_ids: Vec<i32> = data.artists.iter().map(|artist| artist.id).collect();

    let artist_album_counts = match auth.music().get_artist_album_counts_batch(&artist_ids) {
        Ok(counts) => counts,
        Err(error) => {
            return util::repo_error(&auth, error);
        }
    };
    let artist_stars = match search_artist_stars(&auth, &data) {
        Ok(stars) => stars,
        Err(error) => return util::api_error(&auth, &error),
    };

    // Convert to response types with starred status from batch results
    let artist_responses: Vec<ArtistID3Response> = data
        .artists
        .iter()
        .map(|a| {
            let album_count = artist_album_counts.get(&a.id).copied().unwrap_or(0);
            let starred_at = artist_stars.get(&a.id);
            ArtistID3Response::from_artist_with_starred(
                a,
                Some(saturating_i64_to_i32(album_count)),
                starred_at,
            )
        })
        .collect();

    let annotated_albums = match auth
        .music()
        .annotate_albums_for_user(auth.user.id, data.albums)
    {
        Ok(annotated) => annotated,
        Err(error) => return util::repo_error(&auth, error),
    };
    let annotated_songs = match auth
        .music()
        .annotate_songs_for_user(auth.user.id, data.songs)
    {
        Ok(annotated) => annotated,
        Err(error) => return util::repo_error(&auth, error),
    };

    let response = SearchResult3Response {
        artists: artist_responses,
        albums: util::annotate_albums(annotated_albums),
        songs: util::annotate_songs(annotated_songs),
    };

    SubsonicResponse::search_result3(auth.format, response).into_response()
}

/// GET/POST /rest/search2[.view]
///
/// Returns albums, artists and songs matching the given search criteria (non-ID3).
pub async fn search2(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SearchParamsV2>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let limits = search_limits(&params);
    let data = match search_data(&auth, &limits) {
        Ok(data) => data,
        Err(error) => return util::api_error(&auth, &error),
    };

    let artist_stars = match search_artist_stars(&auth, &data) {
        Ok(stars) => stars,
        Err(error) => return util::api_error(&auth, &error),
    };
    let album_stars = match search_album_stars(&auth, &data) {
        Ok(stars) => stars,
        Err(error) => return util::api_error(&auth, &error),
    };

    // Convert to non-ID3 response types
    let artist_responses: Vec<ArtistResponse> = data
        .artists
        .iter()
        .map(|a| {
            let starred_at = artist_stars.get(&a.id);
            ArtistResponse::from_artist_with_starred(a, starred_at)
        })
        .collect();

    let album_responses: Vec<ChildResponse> = data
        .albums
        .iter()
        .map(|a| {
            let starred_at = album_stars.get(&a.id);
            let mut response = ChildResponse::from_album_as_dir(a);
            response.starred = starred_at.map(format_subsonic_datetime);
            response
        })
        .collect();

    let annotated_songs = match auth
        .music()
        .annotate_songs_for_user(auth.user.id, data.songs)
    {
        Ok(annotated) => annotated,
        Err(error) => return util::repo_error(&auth, error),
    };

    let response = SearchResult2Response {
        artists: artist_responses,
        albums: album_responses,
        songs: util::annotate_songs(annotated_songs),
    };

    SubsonicResponse::search_result2(auth.format, response).into_response()
}

/// Query parameters for legacy search.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchParams {
    #[serde(rename = "musicFolderId")]
    pub music_folder_id: Option<i32>,
    /// Artist to search for.
    pub artist: Option<String>,
    /// Album to search for.
    pub album: Option<String>,
    /// Song title to search for.
    pub title: Option<String>,
    /// Searches all fields.
    pub any: Option<String>,
    /// Maximum number of results to return. Default 20.
    pub count: Option<i64>,
    /// Search result offset. Default 0.
    pub offset: Option<i64>,
    /// Only return matches that are newer than this timestamp.
    #[serde(rename = "newerThan")]
    pub newer_than: Option<i64>,
}

/// GET/POST /rest/search[.view]
///
/// Returns a listing of files matching the given search criteria (legacy).
pub async fn search(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SearchParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let count = params.count.unwrap_or(20).clamp(0, 500);
    let offset = params.offset.unwrap_or(0).max(0);

    // Use 'any' field for general search, or combine artist/album/title
    let query = params
        .any
        .as_deref()
        .or(params.title.as_deref())
        .or(params.album.as_deref())
        .or(params.artist.as_deref())
        .unwrap_or("");

    let songs = match auth
        .music()
        .search_songs(query, params.music_folder_id, offset, count)
    {
        Ok(songs) => songs,
        Err(error) => {
            return util::repo_error(&auth, error);
        }
    };

    let matches: Vec<SearchMatch> = songs.iter().map(SearchMatch::from).collect();
    #[expect(
        clippy::cast_possible_wrap,
        reason = "Legacy Subsonic search response requires signed totalHits"
    )]
    let total_hits = matches.len() as i64;

    let response = SearchResultResponse {
        offset,
        total_hits,
        matches,
    };

    SubsonicResponse::search_result(auth.format, response).into_response()
}

#[cfg(test)]
mod tests {
    use super::{SearchParamsV2, search_limits};

    #[test]
    fn search_limits_normalize_wrapping_quotes_and_fold_case() {
        let params = SearchParamsV2 {
            query: Some(" \" Miles Davis \" ".to_string()),
            ..SearchParamsV2::default()
        };

        assert_eq!(search_limits(&params).query, "miles davis");
    }

    #[test]
    fn search_limits_clamp_counts_and_offsets_deterministically() {
        let params = SearchParamsV2 {
            artist_count: Some(999),
            artist_offset: Some(-8),
            album_count: Some(-4),
            album_offset: Some(-1),
            song_count: Some(501),
            song_offset: Some(12),
            ..SearchParamsV2::default()
        };

        let limits = search_limits(&params);

        assert_eq!(limits.artist_count, 500);
        assert_eq!(limits.artist_offset, 0);
        assert_eq!(limits.album_count, 0);
        assert_eq!(limits.album_offset, 0);
        assert_eq!(limits.song_count, 500);
        assert_eq!(limits.song_offset, 12);
    }
}
