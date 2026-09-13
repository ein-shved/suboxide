//! Database schema definitions for Diesel.

diesel::table! {
    users (id) {
        id -> Integer,
        username -> Text,
        password_hash -> Text,
        email -> Nullable<Text>,
        admin_role -> Bool,
        settings_role -> Bool,
        stream_role -> Bool,
        jukebox_role -> Bool,
        download_role -> Bool,
        upload_role -> Bool,
        playlist_role -> Bool,
        cover_art_role -> Bool,
        comment_role -> Bool,
        podcast_role -> Bool,
        share_role -> Bool,
        video_conversion_role -> Bool,
        max_bit_rate -> Integer,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        subsonic_password -> Nullable<Text>,
        api_key -> Nullable<Text>,
        lastfm_session_key -> Nullable<Text>,
    }
}

diesel::table! {
    artist_lastfm_info (artist_id) {
        artist_id -> Integer,
        biography -> Nullable<Text>,
        last_fm_url -> Nullable<Text>,
        small_image_url -> Nullable<Text>,
        medium_image_url -> Nullable<Text>,
        large_image_url -> Nullable<Text>,
        similar_artists -> Nullable<Text>,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    music_folders (id) {
        id -> Integer,
        name -> Text,
        path -> Text,
        enabled -> Bool,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    artists (id) {
        id -> Integer,
        name -> Text,
        sort_name -> Nullable<Text>,
        musicbrainz_id -> Nullable<Text>,
        cover_art -> Nullable<Text>,
        artist_image_url -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    albums (id) {
        id -> Integer,
        name -> Text,
        sort_name -> Nullable<Text>,
        artist_id -> Nullable<Integer>,
        artist_name -> Nullable<Text>,
        year -> Nullable<Integer>,
        genre -> Nullable<Text>,
        cover_art -> Nullable<Text>,
        musicbrainz_id -> Nullable<Text>,
        duration -> Integer,
        song_count -> Integer,
        play_count -> Integer,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    songs (id) {
        id -> Integer,
        title -> Text,
        file_id -> Integer,
        sort_name -> Nullable<Text>,
        album_id -> Nullable<Integer>,
        artist_id -> Nullable<Integer>,
        artist_name -> Nullable<Text>,
        album_name -> Nullable<Text>,
        content_type -> Text,
        suffix -> Text,
        duration -> Integer,
        bit_rate -> Nullable<Integer>,
        bit_depth -> Nullable<Integer>,
        sampling_rate -> Nullable<Integer>,
        channel_count -> Nullable<Integer>,
        track_number -> Nullable<Integer>,
        disc_number -> Nullable<Integer>,
        year -> Nullable<Integer>,
        genre -> Nullable<Text>,
        cover_art -> Nullable<Text>,
        musicbrainz_id -> Nullable<Text>,
        play_count -> Integer,
    }
}

diesel::table! {
    files (id) {
        id -> Integer,
        path -> Text,
        parent_path -> Text,
        size -> BigInt,
        music_folder_id -> Integer,
        content_type -> Text,
        modified_at -> Nullable<BigInt>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        master -> Nullable<Integer>,
    }
}

diesel::table! {
    starred (id) {
        id -> Integer,
        user_id -> Integer,
        artist_id -> Nullable<Integer>,
        album_id -> Nullable<Integer>,
        song_id -> Nullable<Integer>,
        starred_at -> Timestamp,
    }
}

diesel::table! {
    now_playing (id) {
        id -> Integer,
        user_id -> Integer,
        song_id -> Integer,
        player_id -> Nullable<Text>,
        started_at -> Timestamp,
        minutes_ago -> Integer,
    }
}

diesel::table! {
    scrobbles (id) {
        id -> Integer,
        user_id -> Integer,
        song_id -> Integer,
        played_at -> Timestamp,
        submission -> Bool,
    }
}

diesel::table! {
    user_ratings (id) {
        id -> Integer,
        user_id -> Integer,
        song_id -> Nullable<Integer>,
        album_id -> Nullable<Integer>,
        artist_id -> Nullable<Integer>,
        rating -> Integer,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    playlists (id) {
        id -> Integer,
        user_id -> Integer,
        name -> Text,
        comment -> Nullable<Text>,
        public -> Bool,
        song_count -> Integer,
        duration -> Integer,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    playlist_songs (id) {
        id -> Integer,
        playlist_id -> Integer,
        song_id -> Integer,
        position -> Integer,
        created_at -> Timestamp,
    }
}

diesel::table! {
    play_queue (id) {
        id -> Integer,
        user_id -> Integer,
        current_song_id -> Nullable<Integer>,
        position -> Nullable<BigInt>,
        changed_at -> Timestamp,
        changed_by -> Nullable<Text>,
    }
}

diesel::table! {
    play_queue_songs (id) {
        id -> Integer,
        play_queue_id -> Integer,
        song_id -> Integer,
        position -> Integer,
    }
}

// Define foreign key relationships
diesel::joinable!(albums -> artists (artist_id));
diesel::joinable!(artist_lastfm_info -> artists (artist_id));
diesel::joinable!(songs -> albums (album_id));
diesel::joinable!(songs -> artists (artist_id));
diesel::joinable!(songs -> files (file_id));
diesel::joinable!(files -> music_folders (music_folder_id));
diesel::joinable!(starred -> users (user_id));
diesel::joinable!(now_playing -> users (user_id));
diesel::joinable!(now_playing -> songs (song_id));
diesel::joinable!(scrobbles -> users (user_id));
diesel::joinable!(scrobbles -> songs (song_id));
diesel::joinable!(user_ratings -> users (user_id));
diesel::joinable!(playlists -> users (user_id));
diesel::joinable!(playlist_songs -> playlists (playlist_id));
diesel::joinable!(playlist_songs -> songs (song_id));
diesel::joinable!(play_queue -> users (user_id));
diesel::joinable!(play_queue_songs -> play_queue (play_queue_id));
diesel::joinable!(play_queue_songs -> songs (song_id));

diesel::allow_tables_to_appear_in_same_query!(
    users,
    music_folders,
    artists,
    albums,
    songs,
    files,
    starred,
    now_playing,
    scrobbles,
    user_ratings,
    playlists,
    playlist_songs,
    play_queue,
    play_queue_songs,
    artist_lastfm_info,
);
