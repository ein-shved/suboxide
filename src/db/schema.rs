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
        parent -> Nullable<Integer>,
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
        search_name -> Text,
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
        search_name -> Text,
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
        sort_name -> Nullable<Text>,
        album_id -> Nullable<Integer>,
        artist_id -> Nullable<Integer>,
        artist_name -> Nullable<Text>,
        album_name -> Nullable<Text>,
        music_folder_id -> Integer,
        path -> Text,
        parent_path -> Text,
        file_size -> BigInt,
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
        search_name -> Text,
        play_count -> Integer,
        file_modified_at -> Nullable<BigInt>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
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

diesel::table! {
    settings (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    bookmarks (id) {
        id -> Integer,
        user_id -> Integer,
        song_id -> Integer,
        position -> BigInt,
        comment -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    internet_radio_stations (id) {
        id -> Integer,
        name -> Text,
        stream_url -> Text,
        home_page_url -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    remote_sessions (id) {
        id -> Integer,
        session_id -> Text,
        pairing_code -> Text,
        owner_user_id -> Integer,
        host_device_id -> Text,
        host_device_name -> Nullable<Text>,
        controller_user_id -> Nullable<Integer>,
        controller_device_id -> Nullable<Text>,
        controller_device_name -> Nullable<Text>,
        expires_at -> Timestamp,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        closed_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    remote_commands (id) {
        id -> BigInt,
        session_id -> Text,
        source_device_id -> Text,
        command -> Text,
        payload -> Nullable<Text>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    remote_state (session_id) {
        session_id -> Text,
        state_json -> Text,
        updated_by_device_id -> Text,
        updated_at -> Timestamp,
    }
}

// Define foreign key relationships
diesel::joinable!(albums -> artists (artist_id));
diesel::joinable!(artist_lastfm_info -> artists (artist_id));
diesel::joinable!(songs -> albums (album_id));
diesel::joinable!(songs -> artists (artist_id));
diesel::joinable!(songs -> music_folders (music_folder_id));
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
diesel::joinable!(bookmarks -> users (user_id));
diesel::joinable!(bookmarks -> songs (song_id));
diesel::joinable!(remote_sessions -> users (owner_user_id));

diesel::allow_tables_to_appear_in_same_query!(
    users,
    music_folders,
    artists,
    albums,
    songs,
    starred,
    now_playing,
    scrobbles,
    user_ratings,
    playlists,
    playlist_songs,
    play_queue,
    play_queue_songs,
    artist_lastfm_info,
    settings,
    bookmarks,
    internet_radio_stations,
    remote_sessions,
    remote_commands,
    remote_state,
);
