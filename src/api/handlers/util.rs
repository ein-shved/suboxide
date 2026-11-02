//! Shared handler response helpers.

use axum::response::{IntoResponse, Response};

use crate::api::auth::SubsonicContext;
use crate::api::error::ApiError;
use crate::api::response::error_response;
use crate::db::{MusicRepoError, UserRepoError};
use crate::models::music::{AlbumID3Response, ChildResponse};

/// Build a missing-parameter response for the current request format.
pub(in crate::api::handlers) fn missing_param(auth: &SubsonicContext, name: &str) -> Response {
    api_error(auth, &ApiError::MissingParameter(name.into()))
}

/// Build a not-found response for the current request format.
pub(in crate::api::handlers) fn not_found(auth: &SubsonicContext, resource: &str) -> Response {
    api_error(auth, &ApiError::NotFound(resource.into()))
}

/// Build a not-authorized response for the current request format.
pub(in crate::api::handlers) fn unauthorized(auth: &SubsonicContext) -> Response {
    api_error(auth, &ApiError::NotAuthorized)
}

/// Build a generic service error response for the current request format.
pub(in crate::api::handlers) fn service_error(
    auth: &SubsonicContext,
    error: impl std::fmt::Display,
) -> Response {
    api_error(auth, &ApiError::Generic(error.to_string()))
}

/// Map a repository error to a formatted API error response.
///
/// Uses the centralized `From<MusicRepoError> for ApiError` conversion so
/// domain errors (e.g. `NotFound`) map to their proper Subsonic codes
/// instead of leaking as generic errors.
pub(in crate::api::handlers) fn repo_error(
    auth: &SubsonicContext,
    error: MusicRepoError,
) -> Response {
    api_error(auth, &ApiError::from(error))
}

/// Map a user repository error to a formatted API error response.
///
/// Uses the centralized `From<UserRepoError> for ApiError` conversion so
/// domain errors (e.g. `NotFound`) map to their proper Subsonic codes.
pub(in crate::api::handlers) fn user_repo_error(
    auth: &SubsonicContext,
    error: UserRepoError,
) -> Response {
    api_error(auth, &ApiError::from(error))
}

/// Build a formatted API error response.
pub(in crate::api::handlers) fn api_error(auth: &SubsonicContext, error: &ApiError) -> Response {
    error_response(auth.format, error).into_response()
}

/// Run a synchronous library/database operation off the async executor.
///
/// The closure is spawned onto a blocking thread (Diesel and `SQLite` are
/// synchronous). Errors are converted to [`ApiError`] via the `Into` bound,
/// so repository errors keep their structured codes instead of collapsing
/// into a generic message.
pub(in crate::api::handlers) async fn run_blocking<T, F, E>(operation: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Into<ApiError> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(operation).await;
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.into()),
        Err(join_error) => Err(ApiError::Generic(join_error.to_string())),
    }
}

/// Parse API song ids (`mf-N` or bare integers), erroring on the first
/// invalid value.
pub(in crate::api::handlers) fn parse_song_ids(
    ids: &[String],
    param_name: &str,
) -> Result<Vec<i32>, ApiError> {
    ids.iter()
        .map(|id| {
            crate::models::music::EntityId::parse_song(id)
                .ok_or_else(|| ApiError::Generic(format!("Invalid {param_name}: {id}")))
        })
        .collect()
}

/// Build song responses from domain projections with starred, rating,
/// last-played, and bookmark data attached for the current user.
pub(in crate::api::handlers) fn annotate_songs(
    annotated: Vec<crate::api::services::AnnotatedSong>,
) -> Vec<ChildResponse> {
    annotated
        .into_iter()
        .map(|item| {
            ChildResponse::from_song_with_starred(&item.song, item.annotations.starred_at.as_ref())
                .with_annotations(Some(&item.annotations))
        })
        .collect()
}

/// Build album responses from domain projections with starred, rating, and
/// last-played data attached for the current user.
pub(in crate::api::handlers) fn annotate_albums(
    annotated: Vec<crate::api::services::AnnotatedAlbum>,
) -> Vec<AlbumID3Response> {
    annotated
        .into_iter()
        .map(|item| {
            AlbumID3Response::from_album_with_starred(&item.album, item.starred_at.as_ref())
                .with_annotations(Some(&item.annotations))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::annotate_albums;
    use crate::api::services::{AnnotatedAlbum, AnnotatedSong};
    use crate::models::music::{Album, AlbumAnnotations, Song, SongAnnotations};
    use chrono::NaiveDate;

    fn song(id: i32) -> Song {
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .expect("valid date")
            .and_hms_opt(0, 0, 0)
            .expect("valid time");
        Song {
            id,
            title: format!("Song {id}"),
            sort_name: None,
            album_id: Some(1),
            artist_id: Some(1),
            artist_name: Some("Artist".into()),
            album_name: Some("Album".into()),
            music_folder_id: 1,
            path: format!("/music/song{id}.flac"),
            cue_track_id: None,
            parent_path: "/music".into(),
            file_size: 100,
            content_type: "audio/flac".into(),
            suffix: "flac".into(),
            duration: 60,
            bit_rate: None,
            bit_depth: None,
            sampling_rate: None,
            channel_count: None,
            track_number: Some(1),
            disc_number: Some(1),
            year: None,
            genre: None,
            cover_art: None,
            musicbrainz_id: None,
            play_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn album(id: i32) -> Album {
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .expect("valid date")
            .and_hms_opt(0, 0, 0)
            .expect("valid time");
        Album {
            id,
            name: format!("Album {id}"),
            sort_name: None,
            artist_id: Some(1),
            artist_name: Some("Artist".into()),
            year: None,
            genre: None,
            cover_art: None,
            musicbrainz_id: None,
            duration: 120,
            song_count: 2,
            play_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn annotate_songs_maps_projection_to_child_response() {
        let starred = NaiveDate::from_ymd_opt(2024, 2, 2)
            .expect("valid date")
            .and_hms_opt(3, 4, 5)
            .expect("valid time");
        let annotations = SongAnnotations {
            starred_at: Some(starred),
            user_rating: Some(4),
            bookmark_position: Some(900),
            ..SongAnnotations::default()
        };

        let responses = super::annotate_songs(vec![AnnotatedSong {
            song: song(7),
            annotations,
        }]);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, "mf-7");
        assert_eq!(responses[0].user_rating, Some(4));
        assert_eq!(responses[0].bookmark_position, Some(900));
        assert_eq!(
            responses[0].starred.as_deref(),
            Some("2024-02-02T03:04:05.000Z")
        );
    }

    #[test]
    fn annotate_albums_maps_projection_to_album_response() {
        let starred = NaiveDate::from_ymd_opt(2024, 2, 2)
            .expect("valid date")
            .and_hms_opt(3, 4, 5)
            .expect("valid time");
        let annotations = AlbumAnnotations {
            user_rating: Some(5),
            ..AlbumAnnotations::default()
        };

        let responses = annotate_albums(vec![AnnotatedAlbum {
            album: album(3),
            annotations,
            starred_at: Some(starred),
        }]);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, "al-3");
        assert_eq!(responses[0].user_rating, Some(5));
        assert_eq!(
            responses[0].starred.as_deref(),
            Some("2024-02-02T03:04:05.000Z")
        );
    }
}
