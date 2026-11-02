//! List-based browsing handlers (albums, genres, songs).

use axum::response::IntoResponse;
use serde::Deserialize;

use crate::api::auth::SubsonicContext;
use crate::api::error::ApiError;
use crate::api::handlers::util;
use crate::api::response::SubsonicResponse;
use crate::api::services::MusicLibrary;
use crate::db::MusicRepoError;
use crate::models::music::{
    Album, AlbumList2Response, AlbumListResponse, ArtistResponse, ChildResponse, GenreResponse,
    GenresResponse, RandomSongsResponse, SimilarSongs2Response, SimilarSongsResponse, Song,
    SongsByGenreResponse, StarredResponse, TopSongsResponse, format_subsonic_datetime,
};

fn album_list_type_for_request(list_type: Option<&str>) -> &str {
    match list_type {
        None | Some("" | "all") => "alphabeticalByName",
        Some(list_type) => list_type,
    }
}

fn albums_for_list_type(
    auth: &SubsonicContext,
    params: &AlbumList2Params,
    list_type: &str,
    size: i64,
    offset: i64,
) -> Result<Vec<Album>, ApiError> {
    let albums = match list_type {
        "random" => auth.music().get_albums_random(size, params.music_folder_id),
        "newest" => auth
            .music()
            .get_albums_newest(offset, size, params.music_folder_id),
        "frequent" => auth
            .music()
            .get_albums_frequent(offset, size, params.music_folder_id),
        "recent" => auth
            .music()
            .get_albums_recent(offset, size, params.music_folder_id),
        "alphabeticalByName" => {
            auth.music()
                .get_albums_alphabetical_by_name(offset, size, params.music_folder_id)
        }
        "alphabeticalByArtist" => {
            auth.music()
                .get_albums_alphabetical_by_artist(offset, size, params.music_folder_id)
        }
        "byYear" => {
            let from_year = params.from_year.unwrap_or(0);
            let to_year = params.to_year.unwrap_or(9999);
            auth.music().get_albums_by_year(
                from_year,
                to_year,
                offset,
                size,
                params.music_folder_id,
            )
        }
        "byGenre" => {
            let Some(genre) = params.genre.as_deref() else {
                return Err(ApiError::MissingParameter("genre".into()));
            };
            auth.music()
                .get_albums_by_genre(genre, offset, size, params.music_folder_id)
        }
        "starred" => {
            auth.music()
                .get_albums_starred(auth.user.id, offset, size, params.music_folder_id)
        }
        "highest" => {
            auth.music()
                .get_albums_highest(auth.user.id, offset, size, params.music_folder_id)
        }
        _ => {
            return Err(ApiError::Generic(format!("Unknown list type: {list_type}")));
        }
    }?;
    Ok(albums)
}

#[derive(Clone, Copy, Debug)]
enum SimilarSongsSeed {
    Song(i32),
    Album(i32),
    Artist(i32),
}

fn similar_songs_for_seed(
    music: &MusicLibrary,
    seed: SimilarSongsSeed,
    count: i64,
) -> Result<Option<Vec<Song>>, MusicRepoError> {
    match seed {
        SimilarSongsSeed::Song(song_id) => {
            let Some(song) = music.get_song(song_id)? else {
                return Ok(None);
            };
            if let Some(artist_id) = song.artist_id {
                return music
                    .get_similar_songs_by_artist(artist_id, song_id, count)
                    .map(Some);
            }
            if let Some(genre) = song.genre {
                return music.get_songs_by_genre(&genre, count, 0, None).map(Some);
            }
            Ok(Some(Vec::new()))
        }
        SimilarSongsSeed::Album(album_id) => {
            let Some(album) = music.get_album(album_id)? else {
                return Ok(None);
            };
            album.artist_id.map_or_else(
                || Ok(Some(Vec::new())),
                |artist_id| {
                    music
                        .get_similar_songs_by_artist(artist_id, -1, count)
                        .map(Some)
                },
            )
        }
        SimilarSongsSeed::Artist(artist_id) => music
            .get_similar_songs_by_artist(artist_id, -1, count)
            .map(Some),
    }
}

fn similar_songs_seed(params: &SimilarSongs2Params) -> Result<SimilarSongsSeed, ApiError> {
    use crate::models::music::EntityId;

    let seed_count = usize::from(params.id.is_some())
        + usize::from(params.song_id.is_some())
        + usize::from(params.album_id.is_some())
        + usize::from(params.artist_id.is_some());

    match seed_count {
        0 => Err(ApiError::MissingParameter(
            "songId, albumId, or artistId".into(),
        )),
        1 => {
            if let Some(id) = params.id.as_deref() {
                return match EntityId::parse(id) {
                    Some(EntityId::Song(id)) => Ok(SimilarSongsSeed::Song(id)),
                    Some(EntityId::Album(id)) => Ok(SimilarSongsSeed::Album(id)),
                    Some(EntityId::Artist(id)) => Ok(SimilarSongsSeed::Artist(id)),
                    _ => Err(ApiError::Generic("Invalid id".into())),
                };
            }
            if let Some(id) = params.song_id.as_deref() {
                return EntityId::parse_song(id)
                    .map(SimilarSongsSeed::Song)
                    .ok_or_else(|| ApiError::Generic("Invalid songId".into()));
            }
            if let Some(id) = params.album_id.as_deref() {
                return EntityId::parse_album(id)
                    .map(SimilarSongsSeed::Album)
                    .ok_or_else(|| ApiError::Generic("Invalid albumId".into()));
            }
            if let Some(id) = params.artist_id.as_deref() {
                return EntityId::parse_artist(id)
                    .map(SimilarSongsSeed::Artist)
                    .ok_or_else(|| ApiError::Generic("Invalid artistId".into()));
            }
            unreachable!("seed_count verified exactly one seed")
        }
        _ => Err(ApiError::Generic(
            "Specify exactly one of songId, albumId, or artistId".into(),
        )),
    }
}

/// Query parameters for getAlbumList2.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AlbumList2Params {
    /// The list type. Defaults to all albums ordered by name.
    #[serde(rename = "type")]
    pub list_type: Option<String>,
    /// The number of albums to return. Default 10, max 500.
    pub size: Option<i64>,
    /// The list offset. Default 0.
    pub offset: Option<i64>,
    /// The first year in the range (for byYear type).
    #[serde(rename = "fromYear")]
    pub from_year: Option<i32>,
    /// The last year in the range (for byYear type).
    #[serde(rename = "toYear")]
    pub to_year: Option<i32>,
    /// The genre (for byGenre type).
    pub genre: Option<String>,
    /// Only return albums in this music folder.
    #[serde(rename = "musicFolderId")]
    pub music_folder_id: Option<i32>,
}

/// GET/POST /rest/getAlbumList2[.view]
///
/// Returns a list of random, newest, highest rated etc. albums.
/// Similar to getAlbumList, but organizes music according to ID3 tags.
pub async fn get_album_list2(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<AlbumList2Params>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let list_type = album_list_type_for_request(params.list_type.as_deref());
    let size = params.size.unwrap_or(10).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);

    let albums = match albums_for_list_type(&auth, &params, list_type, size, offset) {
        Ok(albums) => albums,
        Err(error) => return util::api_error(&auth, &error),
    };

    let annotated = match auth.music().annotate_albums_for_user(auth.user.id, albums) {
        Ok(annotated) => annotated,
        Err(e) => return util::repo_error(&auth, e),
    };
    let response = AlbumList2Response {
        albums: util::annotate_albums(annotated),
    };

    SubsonicResponse::album_list2(auth.format, response).into_response()
}

/// GET/POST /rest/getAlbumList[.view]
///
/// Returns a list of random, newest, highest rated etc. albums.
/// This is the non-ID3 version that returns Child elements.
pub async fn get_album_list(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<AlbumList2Params>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let list_type = album_list_type_for_request(params.list_type.as_deref());
    let size = params.size.unwrap_or(10).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);

    let albums = match albums_for_list_type(&auth, &params, list_type, size, offset) {
        Ok(albums) => albums,
        Err(error) => return util::api_error(&auth, &error),
    };
    let user_id = auth.user.id;

    // Batch fetch starred status for all albums
    let album_ids: Vec<i32> = albums.iter().map(|a| a.id).collect();
    let starred_map = match auth
        .music()
        .get_starred_at_for_albums_batch(user_id, &album_ids)
    {
        Ok(v) => v,
        Err(e) => {
            return util::repo_error(&auth, e);
        }
    };

    // Convert to Child elements (non-ID3)
    let album_responses: Vec<ChildResponse> = albums
        .iter()
        .map(|a| {
            let starred_at = starred_map.get(&a.id);
            let mut response = ChildResponse::from_album_as_dir(a);
            if let Some(dt) = starred_at {
                response.starred = Some(format_subsonic_datetime(dt));
            }
            response
        })
        .collect();

    let response = AlbumListResponse {
        albums: album_responses,
    };

    SubsonicResponse::album_list(auth.format, response).into_response()
}

/// GET/POST /rest/getGenres[.view]
///
/// Returns all genres.
pub async fn get_genres(auth: SubsonicContext) -> impl IntoResponse {
    let genres = match auth.music().get_genres() {
        Ok(v) => v,
        Err(e) => {
            return util::repo_error(&auth, e);
        }
    };
    let genre_responses: Vec<GenreResponse> = genres
        .into_iter()
        .map(|(name, song_count, album_count)| GenreResponse {
            value: name,
            song_count,
            album_count,
        })
        .collect();

    let response = GenresResponse {
        genres: genre_responses,
    };

    SubsonicResponse::genres(auth.format, response).into_response()
}

/// Query parameters for getRandomSongs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RandomSongsParams {
    /// The number of songs to return. Default 10, max 500.
    pub size: Option<i64>,
    /// Only returns songs belonging to this genre.
    pub genre: Option<String>,
    /// Only return songs published after or in this year.
    #[serde(rename = "fromYear")]
    pub from_year: Option<i32>,
    /// Only return songs published before or in this year.
    #[serde(rename = "toYear")]
    pub to_year: Option<i32>,
    /// Only return songs in this music folder.
    #[serde(rename = "musicFolderId")]
    pub music_folder_id: Option<i32>,
}

/// GET/POST /rest/getRandomSongs[.view]
///
/// Returns random songs matching the given criteria.
pub async fn get_random_songs(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<RandomSongsParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let size = params.size.unwrap_or(10).clamp(1, 500);
    let genre = params.genre.clone();
    let from_year = params.from_year;
    let to_year = params.to_year;
    let music_folder_id = params.music_folder_id;
    let blocking_auth = auth.clone();
    let response = match util::run_blocking(
        move || -> Result<RandomSongsResponse, crate::db::MusicRepoError> {
            let songs = blocking_auth.music().get_random_songs(
                size,
                genre.as_deref(),
                from_year,
                to_year,
                music_folder_id,
            )?;

            let annotated = blocking_auth
                .music()
                .annotate_songs_for_user(blocking_auth.user.id, songs)?;
            Ok(RandomSongsResponse {
                songs: util::annotate_songs(annotated),
            })
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return util::api_error(&auth, &error),
    };

    SubsonicResponse::random_songs(auth.format, response).into_response()
}

/// Query parameters for getSongsByGenre.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SongsByGenreParams {
    /// The genre. Required.
    pub genre: Option<String>,
    /// The number of songs to return. Default 10, max 500.
    pub count: Option<i64>,
    /// The offset. Default 0.
    pub offset: Option<i64>,
    /// Only return songs in this music folder.
    #[serde(rename = "musicFolderId")]
    pub music_folder_id: Option<i32>,
}

/// GET/POST /rest/getSongsByGenre[.view]
///
/// Returns songs in a given genre.
pub async fn get_songs_by_genre(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SongsByGenreParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let Some(genre) = params.genre.clone() else {
        return util::missing_param(&auth, "genre");
    };

    let count = params.count.unwrap_or(10).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);
    let music_folder_id = params.music_folder_id;
    let blocking_auth = auth.clone();
    let response = match util::run_blocking(
        move || -> Result<SongsByGenreResponse, crate::db::MusicRepoError> {
            let songs =
                blocking_auth
                    .music()
                    .get_songs_by_genre(&genre, count, offset, music_folder_id)?;

            let annotated = blocking_auth
                .music()
                .annotate_songs_for_user(blocking_auth.user.id, songs)?;
            Ok(SongsByGenreResponse {
                songs: util::annotate_songs(annotated),
            })
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return util::api_error(&auth, &error),
    };

    SubsonicResponse::songs_by_genre(auth.format, response).into_response()
}

/// Query parameters for getTopSongs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TopSongsParams {
    /// The artist name.
    pub artist: Option<String>,
    /// Max number of songs to return. Default 50.
    pub count: Option<i64>,
}

/// GET/POST /rest/getTopSongs[.view]
///
/// Returns the top songs for a given artist, ordered by play count.
pub async fn get_top_songs(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<TopSongsParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    // Get the required 'artist' parameter
    let artist_name = match params.artist.as_ref() {
        Some(name) if !name.is_empty() => name,
        _ => {
            return util::missing_param(&auth, "artist");
        }
    };

    let count = params.count.unwrap_or(50).clamp(1, 500);

    // Get top songs by artist name (ordered by play count)
    let songs = match auth
        .music()
        .get_top_songs_by_artist_name(artist_name, count)
    {
        Ok(v) => v,
        Err(e) => {
            return util::repo_error(&auth, e);
        }
    };

    let annotated = match auth.music().annotate_songs_for_user(auth.user.id, songs) {
        Ok(annotated) => annotated,
        Err(e) => return util::repo_error(&auth, e),
    };
    let response = TopSongsResponse {
        songs: util::annotate_songs(annotated),
    };

    SubsonicResponse::top_songs(auth.format, response).into_response()
}

/// Query parameters for getSimilarSongs/getSimilarSongs2.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SimilarSongs2Params {
    /// The song, album, or artist ID to use as similarity seed.
    pub id: Option<String>,
    /// The song ID to use as similarity seed.
    #[serde(rename = "songId")]
    pub song_id: Option<String>,
    /// The album ID to use as similarity seed.
    #[serde(rename = "albumId")]
    pub album_id: Option<String>,
    /// The artist ID to use as similarity seed.
    #[serde(rename = "artistId")]
    pub artist_id: Option<String>,
    /// Max number of similar songs to return. Default 50.
    pub count: Option<i64>,
}

/// GET/POST /rest/getSimilarSongs2[.view]
///
/// Returns songs similar to the given song, album, or artist.
/// Since we don't have external metadata, we return random songs from the same artist or genre.
pub async fn get_similar_songs2(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SimilarSongs2Params>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let songs = match similar_songs_response(&auth, &params) {
        Ok(songs) => songs,
        Err(error) => return util::api_error(&auth, &error),
    };
    let response = SimilarSongs2Response { songs };
    SubsonicResponse::similar_songs2(auth.format, response).into_response()
}

/// GET/POST /rest/getSimilarSongs[.view]
///
/// Returns similar songs (non-ID3 version). Similar to getSimilarSongs2.
pub async fn get_similar_songs(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<SimilarSongs2Params>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    let songs = match similar_songs_response(&auth, &params) {
        Ok(songs) => songs,
        Err(error) => return util::api_error(&auth, &error),
    };
    let response = SimilarSongsResponse { songs };
    SubsonicResponse::similar_songs(auth.format, response).into_response()
}

/// Build the annotated similar-songs list shared by the ID3 and legacy
/// endpoints.
fn similar_songs_response(
    auth: &SubsonicContext,
    params: &SimilarSongs2Params,
) -> Result<Vec<ChildResponse>, ApiError> {
    let seed = similar_songs_seed(params)?;
    let count = params.count.unwrap_or(50).clamp(1, 500);

    let songs = similar_songs_for_seed(auth.music(), seed, count)?
        .ok_or_else(|| ApiError::NotFound("Item".into()))?;
    let annotated = auth.music().annotate_songs_for_user(auth.user.id, songs)?;
    Ok(util::annotate_songs(annotated))
}

/// GET/POST /rest/getStarred[.view]
///
/// Returns starred songs, albums and artists (non-ID3 version).
pub async fn get_starred(auth: SubsonicContext) -> impl IntoResponse {
    let items = match auth.music().starred_items_for_user(auth.user.id) {
        Ok(items) => items,
        Err(e) => {
            return util::repo_error(&auth, e);
        }
    };

    // Convert to response types
    let artist_responses: Vec<ArtistResponse> = items
        .artists
        .iter()
        .map(|(artist, starred_at)| {
            ArtistResponse::from_artist_with_starred(artist, Some(starred_at))
        })
        .collect();

    let album_responses: Vec<ChildResponse> = items
        .albums
        .iter()
        .map(|item| {
            let mut response = ChildResponse::from_album_as_dir(&item.album);
            response.starred = item.starred_at.as_ref().map(format_subsonic_datetime);
            response
        })
        .collect();

    let response = StarredResponse {
        artists: artist_responses,
        albums: album_responses,
        songs: util::annotate_songs(items.songs),
    };

    SubsonicResponse::starred(auth.format, response).into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        SimilarSongs2Params, SimilarSongsSeed, album_list_type_for_request, similar_songs_seed,
    };
    use crate::api::error::ApiError;

    #[test]
    fn defaults_missing_album_list_type_to_alphabetical_by_name() {
        assert_eq!(album_list_type_for_request(None), "alphabeticalByName");
        assert_eq!(album_list_type_for_request(Some("")), "alphabeticalByName");
    }

    #[test]
    fn supports_all_album_list_alias() {
        assert_eq!(
            album_list_type_for_request(Some("all")),
            "alphabeticalByName"
        );
    }

    #[test]
    fn preserves_supported_album_list_types() {
        assert_eq!(
            album_list_type_for_request(Some("alphabeticalByArtist")),
            "alphabeticalByArtist"
        );
        assert_eq!(album_list_type_for_request(Some("newest")), "newest");
    }

    #[test]
    fn similar_songs_seed_requires_exactly_one_seed() {
        let missing = similar_songs_seed(&SimilarSongs2Params::default())
            .expect_err("missing seed should fail");
        assert!(
            matches!(missing, ApiError::MissingParameter(parameter) if parameter == "songId, albumId, or artistId")
        );

        let duplicate = similar_songs_seed(&SimilarSongs2Params {
            song_id: Some("mf-1".to_string()),
            album_id: Some("al-2".to_string()),
            artist_id: None,
            id: None,
            count: None,
        })
        .expect_err("multiple seeds should fail");
        assert!(
            matches!(duplicate, ApiError::Generic(message) if message == "Specify exactly one of songId, albumId, or artistId")
        );
    }

    #[test]
    fn similar_songs_seed_accepts_generic_id() {
        let song = similar_songs_seed(&SimilarSongs2Params {
            id: Some("mf-42".to_string()),
            ..SimilarSongs2Params::default()
        })
        .expect("song id should parse");
        assert!(matches!(song, SimilarSongsSeed::Song(42)));

        let album = similar_songs_seed(&SimilarSongs2Params {
            id: Some("al-7".to_string()),
            ..SimilarSongs2Params::default()
        })
        .expect("album id should parse");
        assert!(matches!(album, SimilarSongsSeed::Album(7)));

        let artist = similar_songs_seed(&SimilarSongs2Params {
            id: Some("ar-3".to_string()),
            ..SimilarSongs2Params::default()
        })
        .expect("artist id should parse");
        assert!(matches!(artist, SimilarSongsSeed::Artist(3)));

        let bare = similar_songs_seed(&SimilarSongs2Params {
            id: Some("9".to_string()),
            ..SimilarSongs2Params::default()
        })
        .expect("bare integer should default to song");
        assert!(matches!(bare, SimilarSongsSeed::Song(9)));

        let invalid = similar_songs_seed(&SimilarSongs2Params {
            id: Some("bogus".to_string()),
            ..SimilarSongs2Params::default()
        })
        .expect_err("invalid id should fail");
        assert!(matches!(invalid, ApiError::Generic(message) if message == "Invalid id"));
    }

    #[test]
    fn similar_songs_seed_preserves_selected_seed_id() {
        let seed = similar_songs_seed(&SimilarSongs2Params {
            song_id: None,
            album_id: None,
            artist_id: Some("ar-42".to_string()),
            id: None,
            count: None,
        })
        .expect("single artist seed should parse");

        assert!(matches!(seed, SimilarSongsSeed::Artist(42)));
    }

    #[test]
    fn similar_songs_seed_rejects_invalid_ids() {
        let invalid = similar_songs_seed(&SimilarSongs2Params {
            song_id: Some("bogus".to_string()),
            album_id: None,
            artist_id: None,
            id: None,
            count: None,
        })
        .expect_err("invalid id should fail");
        assert!(matches!(invalid, ApiError::Generic(message) if message == "Invalid songId"));
    }
}
