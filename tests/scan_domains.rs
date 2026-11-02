//! Directory ownership, scoped discovery, and shared album contracts.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use diesel::prelude::*;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, ItemKey};
use suboxide::db::{DbConfig, DbPool, MusicFolderRepository, SongRepository, initialize_database};
use suboxide::models::music::NewMusicFolder;
use suboxide::models::music::Song;
use suboxide::scanner::{ScanError, ScanMode, ScanTarget, Scanner};
use tracing_subscriber::prelude::*;

static SEQUENCE: AtomicU32 = AtomicU32::new(0);
static SCANNER_TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct Library {
    path: PathBuf,
    pool: DbPool,
}

impl Library {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "suboxide-domains-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("fixture directory");
        let pool = DbConfig {
            database_url: ":memory:".into(),
            max_connections: 1,
            ..DbConfig::default()
        }
        .build_pool()
        .expect("pool");
        initialize_database(&mut pool.get().expect("connection")).expect("fresh schema");
        Self { path, pool }
    }

    fn root(&self, name: &str) -> i32 {
        let path = self.path.join(name);
        std::fs::create_dir_all(&path).expect("root directory");
        MusicFolderRepository::new(self.pool.clone())
            .create(&NewMusicFolder {
                name: name.into(),
                path: path.to_string_lossy().into_owned(),
                enabled: true,
            })
            .expect("register root")
            .id
    }

    fn scanner(&self) -> Scanner {
        Scanner::with_cover_art_dir(self.pool.clone(), self.path.join("covers"))
    }

    fn songs(&self) -> Vec<Song> {
        #[derive(QueryableByName)]
        struct Id {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            id: i32,
        }
        let ids = diesel::sql_query("SELECT id FROM songs ORDER BY path")
            .load::<Id>(&mut self.pool.get().expect("connection"))
            .expect("song IDs");
        let songs = SongRepository::new(self.pool.clone());
        ids.into_iter()
            .map(|row| {
                songs
                    .find_by_id(row.id)
                    .expect("song query")
                    .expect("song exists")
            })
            .collect()
    }

    fn folder(&self, root: i32, path: &str) -> suboxide::models::music::MusicFolder {
        MusicFolderRepository::new(self.pool.clone())
            .find(root, path)
            .expect("folder query")
            .expect("folder exists")
    }
}

fn wave(path: &Path, title: &str, seconds: u32) {
    use lofty::config::WriteOptions;
    use lofty::prelude::TagExt;
    use lofty::tag::{Tag, TagType};
    std::fs::create_dir_all(path.parent().expect("containing directory")).expect("directories");
    let size = seconds * 16_000;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(b"\x02\0\x10\0data");
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.resize(44 + usize::try_from(size).expect("small fixture"), 0);
    std::fs::write(path, bytes).expect("wave fixture");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_owned());
    tag.set_artist("Artist".into());
    tag.set_album("Album".into());
    tag.save_to_path(path, WriteOptions::default())
        .expect("wave tags");
}

fn retag(path: &Path, update: impl FnOnce(&mut lofty::tag::Tag)) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::TagExt;
    let mut file = lofty::read_from_path(path).expect("read fixture tags");
    let tag = file.primary_tag_mut().expect("primary tag");
    update(tag);
    tag.save_to_path(path, lofty::config::WriteOptions::default())
        .expect("retag fixture");
}

fn assert_album(library: &Library, id: i32, count: i32, duration: i32) {
    let album = suboxide::db::AlbumRepository::new(library.pool.clone())
        .find_by_id(id)
        .expect("album query")
        .expect("surviving album");
    let members = SongRepository::new(library.pool.clone())
        .find_by_album(id)
        .expect("members");
    assert_eq!(album.song_count, count);
    assert_eq!(album.duration, duration);
    assert_eq!(members.len(), usize::try_from(count).expect("count"));
    assert_eq!(
        members.iter().map(|song| song.duration).sum::<i32>(),
        duration
    );
}

#[test]
fn asd_04_shared_album_reconciliation_and_root_removal() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let first = library.root("first");
    let second = library.root("second");
    let a = library.path.join("first/A/one.wav");
    let b = library.path.join("first/B/two.wav");
    wave(&a, "One", 1);
    wave(&b, "Two", 2);
    wave(&library.path.join("second/nested/three.wav"), "Three", 3);
    let scanner = library.scanner();
    scanner
        .scan(ScanTarget::AllEnabled, ScanMode::Full, None)
        .expect("index");
    let before = library.songs();
    let original = before
        .iter()
        .find(|song| song.title == "One")
        .expect("original");
    let album = original.album_id.expect("shared album");
    assert_album(&library, album, 3, 6);
    let folders = MusicFolderRepository::new(library.pool.clone());
    folders
        .set_enabled(second, false)
        .expect("disable untouched root");
    retag(&a, |tag| tag.set_album("Another album".into()));
    let (_, events) = observe(|| {
        scanner
            .scan(directory(first, "A"), ScanMode::Full, None)
            .expect("retag")
    });
    assert_eq!(
        fields(&events, "scan.track.open", "path"),
        BTreeSet::from([a.to_string_lossy().into_owned()])
    );
    assert_album(&library, album, 2, 5);
    let songs = library.songs();
    let changed = songs
        .iter()
        .find(|song| song.id == original.id)
        .expect("retained song ID");
    let new_album = changed.album_id.expect("new album");
    assert_ne!(album, new_album);
    assert_album(&library, new_album, 1, 1);
    std::fs::remove_file(b).expect("remove member");
    scanner
        .scan(directory(first, "B"), ScanMode::Incremental, None)
        .expect("partial removal");
    assert_album(&library, album, 1, 3);
    assert!(
        folders
            .delete(second)
            .expect("remove disabled root and subtree")
    );
    assert!(
        folders
            .find_by_root(second)
            .expect("removed tree")
            .is_empty()
    );
    assert!(
        suboxide::db::AlbumRepository::new(library.pool.clone())
            .find_by_id(album)
            .expect("query")
            .is_none()
    );
    assert_album(&library, new_album, 1, 1);
    retag(&a, Accessor::remove_album);
    scanner
        .scan(directory(first, "A"), ScanMode::Full, None)
        .expect("remove album membership");
    assert!(library.songs()[0].album_id.is_none());
    assert!(
        suboxide::db::AlbumRepository::new(library.pool.clone())
            .find_by_id(new_album)
            .expect("query")
            .is_none()
    );
    assert!(folders.delete(first).expect("remove last root"));
    assert!(library.songs().is_empty());
}

#[test]
fn asd_03_root_removal_retains_shared_album_in_disabled_root() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let first = library.root("first");
    let second = library.root("second");
    wave(&library.path.join("first/nested/track.wav"), "First", 2);
    wave(&library.path.join("second/nested/track.wav"), "Second", 3);
    library
        .scanner()
        .scan(ScanTarget::AllEnabled, ScanMode::Full, None)
        .expect("index");
    let songs = library.songs();
    let album = songs[0].album_id.expect("shared album");
    let folders = MusicFolderRepository::new(library.pool.clone());
    folders
        .set_enabled(second, false)
        .expect("disable surviving root");
    assert!(folders.delete(first).expect("delete first tree"));
    assert_eq!(library.songs().len(), 1);
    assert_eq!(
        library.songs()[0].id,
        songs
            .iter()
            .find(|song| song.title == "Second")
            .expect("surviving ID")
            .id
    );
    assert_album(&library, album, 1, 3);
}

#[test]
fn asd_04_album_identity_metadata_and_artwork() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("music");
    let a = library.path.join("music/A/track.wav");
    wave(&a, "Original", 1);
    retag(&a, |tag| {
        tag.set_year(1999);
        tag.set_genre("Original genre".into());
    });
    let scanner = library.scanner();
    scanner
        .scan(directory(root, "A"), ScanMode::Full, None)
        .expect("no artwork yet");
    let album_id = library.songs()[0].album_id.expect("album");
    let albums = suboxide::db::AlbumRepository::new(library.pool.clone());
    assert!(
        albums
            .find_by_id(album_id)
            .expect("query")
            .expect("album")
            .cover_art
            .is_none()
    );
    std::fs::write(
        a.parent().expect("parent").join("cover.jpg"),
        b"external alternative",
    )
    .expect("external art");
    let picture = || {
        Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Png),
            None,
            b"embedded artwork".to_vec(),
        )
    };
    retag(&a, |tag| {
        tag.set_year(2024);
        tag.set_genre("Changed".into());
        tag.push_picture(picture());
    });
    scanner
        .scan(directory(root, "A"), ScanMode::Full, None)
        .expect("fill missing cover");
    let album = albums.find_by_id(album_id).expect("query").expect("album");
    assert_eq!(album.year, Some(1999));
    assert_eq!(album.genre.as_deref(), Some("Original genre"));
    let cover = album.cover_art.expect("embedded cover");
    assert_eq!(
        cover,
        suboxide::cover_art::content_hash(b"embedded artwork")
    );
    retag(&a, |tag| {
        tag.remove_picture(0);
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Png),
            None,
            b"changed artwork".to_vec(),
        ));
    });
    scanner
        .scan(directory(root, "A"), ScanMode::Full, None)
        .expect("reuse existing cover");
    assert_eq!(
        albums
            .find_by_id(album_id)
            .expect("query")
            .expect("album")
            .cover_art
            .as_deref(),
        Some(cover.as_str())
    );
}

#[test]
fn asd_04_album_artist_identity_and_external_artwork() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("music");
    let a = library.path.join("music/A/track.wav");
    wave(&a, "Original", 1);
    let scanner = library.scanner();
    scanner
        .scan(directory(root, "A"), ScanMode::Full, None)
        .expect("initial album");
    let album_id = library.songs()[0].album_id.expect("album");
    let albums = suboxide::db::AlbumRepository::new(library.pool.clone());
    for (folder, artist, album_artist) in [
        ("B", Some("Artist"), None),
        ("C", Some("Artist"), Some("Album Artist")),
        ("D", None, None),
    ] {
        let path = library.path.join(format!("music/{folder}/track.wav"));
        wave(&path, folder, 1);
        retag(&path, |tag| {
            if let Some(artist) = artist {
                tag.set_artist(artist.into());
            } else {
                tag.remove_artist();
            }
            if let Some(artist) = album_artist {
                tag.insert_text(ItemKey::AlbumArtist, artist.into());
            }
        });
        std::fs::write(
            path.parent().expect("parent").join("cover.jpg"),
            b"adjacent artwork",
        )
        .expect("external cover");
        scanner
            .scan(directory(root, folder), ScanMode::Full, None)
            .expect("identity case");
    }
    let songs = library.songs();
    assert_eq!(
        songs
            .iter()
            .find(|song| song.title == "B")
            .expect("fallback")
            .album_id,
        Some(album_id)
    );
    let c = songs
        .iter()
        .find(|song| song.title == "C")
        .expect("album artist")
        .album_id
        .expect("album");
    let d = songs
        .iter()
        .find(|song| song.title == "D")
        .expect("missing artist")
        .album_id
        .expect("album");
    assert_ne!(c, album_id);
    assert_ne!(c, d);
    assert_ne!(d, album_id);
    assert_eq!(
        albums
            .find_by_id(d)
            .expect("query")
            .expect("album")
            .artist_id,
        None
    );
    assert_eq!(
        albums
            .find_by_id(c)
            .expect("query")
            .expect("album")
            .cover_art,
        Some(suboxide::cover_art::content_hash(b"adjacent artwork"))
    );
}

#[derive(Debug, Default)]
struct Event {
    name: &'static str,
    fields: HashMap<&'static str, String>,
}

impl tracing::field::Visit for Event {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields.insert(field.name(), format!("{value:?}"));
    }
}

struct TraceSession {
    events: Vec<Event>,
    action: Box<dyn Fn(&Event) + Send + Sync>,
}

static TRACE_SESSION: Mutex<Option<TraceSession>> = Mutex::new(None);
static TRACE_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();

#[derive(Debug)]
struct Capture;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut recorded = Event {
            name: event.metadata().name(),
            ..Event::default()
        };
        event.record(&mut recorded);
        if let Some(session) = TRACE_SESSION.lock().expect("trace session").as_mut() {
            (session.action)(&recorded);
            session.events.push(recorded);
        }
    }
}

fn observe_with<T>(
    action: impl Fn(&Event) + Send + Sync + 'static,
    work: impl FnOnce() -> T,
) -> (T, Vec<Event>) {
    // Keep the dispatcher registered across tests, including Rayon handoffs.
    let dispatch = TRACE_DISPATCH
        .get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry().with(Capture)));
    *TRACE_SESSION.lock().expect("trace session") = Some(TraceSession {
        events: Vec::new(),
        action: Box::new(action),
    });
    let result = tracing::dispatcher::with_default(dispatch, || {
        tracing::callsite::rebuild_interest_cache();
        work()
    });
    let events = TRACE_SESSION
        .lock()
        .expect("trace session")
        .take()
        .expect("active trace")
        .events;
    (result, events)
}

fn observe<T>(work: impl FnOnce() -> T) -> (T, Vec<Event>) {
    observe_with(|_| {}, work)
}

fn fields(events: &[Event], name: &str, field: &str) -> BTreeSet<String> {
    events
        .iter()
        .filter(|event| event.name == name)
        .filter_map(|event| event.fields.get(field).cloned())
        .collect()
}

fn directory(root: i32, path: &str) -> ScanTarget {
    ScanTarget::Directory {
        root_id: root,
        path: path.into(),
    }
}

fn api_app(library: &Library) -> (axum::Router, suboxide::scanner::ScanStateHandle) {
    use suboxide::app::{AppState, CorsConfig, create_router};
    use suboxide::lastfm::LastFmClient;
    diesel::sql_query("INSERT INTO users (id, username, password_hash, admin_role, api_key) VALUES (1, 'admin', 'unused', TRUE, 'admin-key'), (2, 'listener', 'unused', FALSE, 'listener-key')")
        .execute(&mut library.pool.get().expect("connection")).expect("API users");
    let state = AppState::new(
        library.pool.clone(),
        LastFmClient::new(String::new(), String::new()).expect("disabled Last.fm"),
    );
    let scan_state = state.scan_state();
    (create_router(state, &CorsConfig::default()), scan_state)
}

async fn api(app: &axum::Router, endpoint: &str, query: &str, key: &str) -> serde_json::Value {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .uri(format!(
            "/rest/{endpoint}?apiKey={key}&v=1.16.1&c=test&f=json&{query}"
        ))
        .body(axum::body::Body::empty())
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON response");
    value["subsonic-response"].take()
}

async fn finish_scan(state: &suboxide::scanner::ScanStateHandle) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.is_scanning() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("background scan finishes");
}

#[test]
fn asd_03_api_scan_selection_and_errors() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("music");
    let other = library.root("other");
    for name in [
        "music/root.wav",
        "music/A/track.wav",
        "music/A/child/track.wav",
        "music/B/track.wav",
        "other/track.wav",
    ] {
        let path = library.path.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
        std::fs::write(path, b"invalid metadata fixture").expect("audio entry");
    }
    let generated = MusicFolderRepository::new(library.pool.clone())
        .ensure(root, "A")
        .expect("generated")
        .id;
    let (app, state) = api_app(&library);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            for (query, code) in [
                ("directory=A".to_owned(), 10),
                ("musicFolderId=99999".into(), 70),
                (format!("musicFolderId={generated}"), 70),
                (format!("musicFolderId={root}&directory=../outside"), 0),
            ] {
                assert_eq!(
                    api(&app, "startScan", &query, "admin-key").await["error"]["code"],
                    code
                );
                assert!(!state.is_scanning());
            }
            MusicFolderRepository::new(library.pool.clone())
                .set_enabled(other, false)
                .expect("disable other");
            assert_eq!(
                api(
                    &app,
                    "startScan",
                    &format!("musicFolderId={other}"),
                    "admin-key"
                )
                .await["error"]["code"],
                0
            );
            MusicFolderRepository::new(library.pool.clone())
                .set_enabled(other, true)
                .expect("enable other");
            assert_eq!(
                api(&app, "startScan", "", "listener-key").await["error"]["code"],
                50
            );
            let guard = state.try_start().expect("busy scan");
            state.set_count(17);
            let busy = api(&app, "startScan", "directory=without-root", "admin-key").await;
            assert_eq!(busy["scanStatus"]["count"], 17);
            drop(guard);
            for full in [false, true] {
                for (selector, total) in [
                    (String::new(), 5),
                    (format!("musicFolderId={root}"), 4),
                    (format!("musicFolderId={root}&directory=A//./"), 1),
                    (format!("musicFolderId={root}&directory=."), 1),
                    (format!("musicFolderId={root}&directory=missing"), 0),
                ] {
                    let response = api(
                        &app,
                        "startScan",
                        &format!("{selector}&fullScan={full}"),
                        "admin-key",
                    )
                    .await;
                    assert_eq!(response["status"], "ok");
                    finish_scan(&state).await;
                    assert_eq!(state.get_total(), total);
                    let status = api(&app, "getScanStatus", "", "admin-key").await;
                    assert_eq!(status["scanStatus"]["scanning"], false);
                    assert_eq!(status["scanStatus"]["total"].as_u64().unwrap_or(0), total);
                }
            }
        });
}

#[test]
fn asd_03_public_filters_before_pagination() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let first = library.root("first");
    let second = library.root("second");
    for (root, label) in [("first", "Z"), ("second", "A")] {
        for number in [1, 2] {
            let path = library.path.join(format!("{root}/nested/{number}.wav"));
            wave(&path, &format!("{label}{number}"), number);
            retag(&path, |tag| {
                tag.set_artist(format!("{label}{number}"));
                tag.set_album(format!("{label}{number}"));
                tag.set_year(2000);
                tag.set_genre("Genre".into());
            });
        }
    }
    library
        .scanner()
        .scan(ScanTarget::AllEnabled, ScanMode::Full, None)
        .expect("index");
    let generated = library.folder(first, "nested").id;
    let songs = SongRepository::new(library.pool.clone());
    let albums = suboxide::db::AlbumRepository::new(library.pool.clone());
    let artists = suboxide::db::ArtistRepository::new(library.pool.clone());
    assert_eq!(
        songs.search("", Some(first), 1, 1).expect("song page")[0].title,
        "Z2"
    );
    assert_eq!(
        albums.search("", Some(first), 1, 1).expect("album page")[0].name,
        "Z2"
    );
    assert_eq!(
        artists.search("", Some(first), 1, 1).expect("artist page")[0].name,
        "Z2"
    );
    for invalid in [generated, 99999] {
        assert!(
            songs
                .find_by_music_folder(invalid)
                .expect("invalid root")
                .is_empty()
        );
        assert!(
            albums
                .search("", Some(invalid), 0, 20)
                .expect("invalid albums")
                .is_empty()
        );
        assert!(
            artists
                .search("", Some(invalid), 0, 20)
                .expect("invalid artists")
                .is_empty()
        );
    }
    MusicFolderRepository::new(library.pool.clone())
        .set_enabled(second, false)
        .expect("disabled tracks stay visible");
    assert_eq!(
        songs
            .search("", None, 0, 20)
            .expect("unfiltered songs")
            .len(),
        4
    );
    let (app, _) = api_app(&library);
    diesel::connection::SimpleConnection::batch_execute(&mut *library.pool.get().expect("connection"), "UPDATE albums SET play_count = 1; INSERT INTO starred (user_id, album_id, starred_at) SELECT 1, id, CASE name WHEN 'Z1' THEN '2024-01-02 00:00:00' WHEN 'Z2' THEN '2024-01-01 00:00:00' ELSE '2025-01-01 00:00:00' END FROM albums; INSERT INTO user_ratings (user_id, album_id, rating) SELECT 1, id, CASE name WHEN 'Z1' THEN 4 WHEN 'Z2' THEN 3 ELSE 5 END FROM albums;").expect("album lists");
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
        let folders = api(&app, "getMusicFolders", "", "admin-key").await;
        assert_eq!(folders["musicFolders"]["musicFolder"].as_array().expect("roots").len(), 1);
        for endpoint in ["getIndexes", "getArtists", "getAlbumList", "getAlbumList2", "search", "search2", "search3", "getRandomSongs"] {
            let response = api(&app, endpoint, &format!("musicFolderId={first}&size=1&offset=1&songOffset=1&songCount=1&albumOffset=1&albumCount=1&artistOffset=1&artistCount=1&count=1"), "admin-key").await;
            let encoded = response.to_string();
            assert!(encoded.contains("Z2") || (endpoint == "getRandomSongs" && encoded.contains("Z1")), "{endpoint}: {encoded}");
            assert!(!encoded.contains("A1") && !encoded.contains("A2"), "{endpoint}: {encoded}");
            let invalid = api(&app, endpoint, &format!("musicFolderId={generated}"), "admin-key").await.to_string();
            assert!(!invalid.contains("Z1") && !invalid.contains("Z2"), "{endpoint}: {invalid}");
        }
        let directory = api(&app, "getMusicDirectory", &format!("id={first}"), "admin-key").await.to_string();
        assert!(directory.contains("Z1") && directory.contains("Z2"));
        assert!(!directory.contains("A1"));
        assert_eq!(api(&app, "getMusicDirectory", &format!("id={generated}"), "admin-key").await["error"]["code"], 70);
        assert_album_list_filters(&app, first, generated).await;
    });
}

#[test]
fn asd_01_direct_update_isolation() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [ScanMode::Incremental, ScanMode::Full] {
        let library = Library::new();
        let root = library.root("music");
        let base = library.path.join("music");
        for name in ["A/change", "A/keep", "A/remove", "A/child/track", "B/track"] {
            wave(&base.join(format!("{name}.wav")), name, 1);
        }
        let scanner = library.scanner();
        scanner
            .scan(ScanTarget::Root(root), ScanMode::Full, None)
            .expect("initial index");
        let before = library.songs();
        let owner = library.folder(root, "A");
        wave(&base.join("A/change.wav"), "Changed title and duration", 2);
        wave(&base.join("A/new.wav"), "New", 3);
        std::fs::remove_file(base.join("A/remove.wav")).expect("remove track");
        let (result, events) = observe(|| {
            scanner
                .scan(directory(root, "A"), mode, None)
                .expect("direct update")
        });
        assert_eq!(result.tracks_added, 1);
        assert_eq!(result.tracks_removed, 1);
        assert_eq!(
            fields(&events, "scan.directory.enumerate", "path"),
            BTreeSet::from([owner.path.clone()])
        );
        assert_eq!(
            fields(&events, "scan.directory.read", "owner"),
            BTreeSet::from([owner.id.to_string()])
        );
        assert_eq!(
            fields(&events, "scan.directory.member", "returned_owner"),
            BTreeSet::from([owner.id.to_string()])
        );
        assert_eq!(
            fields(&events, "scan.directory.reconcile", "owner"),
            BTreeSet::from([owner.id.to_string()])
        );
        let expected = if mode == ScanMode::Full {
            vec!["change", "keep", "new"]
        } else {
            vec!["change", "new"]
        };
        assert_eq!(
            fields(&events, "scan.track.open", "path"),
            expected
                .iter()
                .map(|name| base
                    .join(format!("A/{name}.wav"))
                    .to_string_lossy()
                    .into_owned())
                .collect()
        );
        let after = library.songs();
        for song in &before {
            if song.music_folder_id != owner.id
                || (mode == ScanMode::Incremental && song.title == "A/keep")
            {
                let unchanged = after
                    .iter()
                    .find(|row| row.id == song.id)
                    .expect("unvisited song");
                assert_eq!(format!("{unchanged:?}"), format!("{song:?}"));
            }
        }
        let changed = after
            .iter()
            .find(|song| song.title == "Changed title and duration")
            .expect("retagged");
        assert_eq!(
            changed.id,
            before
                .iter()
                .find(|song| song.title == "A/change")
                .expect("old track")
                .id
        );
        assert_eq!(changed.music_folder_id, owner.id);
    }
}

#[test]
fn asd_02_recursive_hierarchy_and_proven_absence() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("music");
    let base = library.path.join("music");
    wave(&base.join("A/direct.wav"), "Direct", 1);
    wave(&base.join("A/child/track.wav"), "Child", 1);
    std::fs::create_dir_all(base.join("images/deeper")).expect("image-only directories");
    std::fs::write(base.join("images/cover.jpg"), b"image").expect("image");
    std::os::unix::fs::symlink(base.join("A"), base.join("link")).expect("directory symlink");
    let scanner = library.scanner();
    scanner
        .scan(ScanTarget::Root(root), ScanMode::Full, None)
        .expect("index");
    assert!(
        MusicFolderRepository::new(library.pool.clone())
            .find(root, "images")
            .expect("query")
            .is_none()
    );
    wave(&base.join("images/deeper/new.wav"), "New nested audio", 1);
    wave(&base.join("new/discovered.wav"), "Explicit discovery", 1);
    scanner
        .scan(directory(root, "new"), ScanMode::Incremental, None)
        .expect("unstored directory");
    let folders = MusicFolderRepository::new(library.pool.clone());
    let a = library.folder(root, "A");
    folders.set_enabled(root, true).expect("root enabled");
    diesel::sql_query("UPDATE music_folders SET enabled = FALSE WHERE id = ?")
        .bind::<diesel::sql_types::Integer, _>(a.id)
        .execute(&mut library.pool.get().expect("connection"))
        .expect("generated enabled ignored");
    for mode in [ScanMode::Incremental, ScanMode::Full] {
        for target in [ScanTarget::Root(root), ScanTarget::AllEnabled] {
            let (_, events) = observe(|| scanner.scan(target, mode, None).expect("recursive scan"));
            let songs = library.songs();
            let owners: BTreeSet<_> = songs
                .iter()
                .map(|song| song.music_folder_id.to_string())
                .collect();
            assert!(owners.is_subset(&fields(&events, "scan.directory.read", "owner")));
            assert!(owners.is_subset(&fields(&events, "scan.directory.reconcile", "owner")));
            for event in events
                .iter()
                .filter(|event| event.name == "scan.directory.member")
            {
                assert_eq!(event.fields["owner"], event.fields["returned_owner"]);
            }
            assert!(
                !fields(&events, "scan.directory.enumerate", "path")
                    .contains(&base.join("link").to_string_lossy().into_owned())
            );
        }
    }
    std::fs::remove_dir_all(base.join("A")).expect("missing branch");
    let result = scanner
        .scan(directory(root, "A"), ScanMode::Incremental, None)
        .expect("selected absence");
    assert_eq!(result.tracks_removed, 1);
    assert!(library.songs().iter().any(|song| song.title == "Child"));
    assert_eq!(library.folder(root, "A").id, a.id);
    scanner
        .scan(ScanTarget::Root(root), ScanMode::Incremental, None)
        .expect("recursive absence");
    assert!(!library.songs().iter().any(|song| song.title == "Child"));
    assert!(folders.find(root, "A").expect("query").is_none());
    let before = format!("{:?}", library.songs());
    std::fs::rename(&base, library.path.join("offline")).expect("offline root");
    assert!(
        scanner
            .scan(ScanTarget::Root(root), ScanMode::Full, None)
            .is_err()
    );
    assert_eq!(format!("{:?}", library.songs()), before);
}

#[test]
fn asd_03_target_resolution() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let scanner = library.scanner();
    assert!(matches!(
        scanner.resolve_target(ScanTarget::AllEnabled),
        Err(ScanError::NoMusicFolders)
    ));
    let root = library.root("music");
    let folders = MusicFolderRepository::new(library.pool.clone());
    let child = folders.ensure(root, "A").expect("child");
    std::fs::create_dir_all(&child.path).expect("directory");
    std::fs::write(library.path.join("music/file"), b"file").expect("file");
    std::os::unix::fs::symlink(&child.path, library.path.join("music/link")).expect("symlink");
    for (input, normalized) in [
        ("A//./", "A"),
        (".", ""),
        ("", ""),
        ("missing/deeper", "missing/deeper"),
    ] {
        assert_eq!(
            scanner
                .resolve_target(directory(root, input))
                .expect("valid target"),
            directory(root, normalized)
        );
    }
    for invalid in [
        "/absolute",
        "../outside",
        "A/../A",
        "link",
        "file",
        "file/child",
    ] {
        assert!(matches!(
            scanner.resolve_target(directory(root, invalid)),
            Err(ScanError::InvalidTarget(_))
        ));
    }
    assert!(matches!(
        scanner.resolve_target(directory(root, &"x".repeat(300))),
        Err(ScanError::Io(_))
    ));
    for invalid in [child.id, i32::MAX] {
        assert!(matches!(
            scanner.resolve_target(ScanTarget::Root(invalid)),
            Err(ScanError::FolderNotFound(_))
        ));
    }
    folders.set_enabled(root, false).expect("disable");
    assert!(matches!(
        scanner.resolve_target(ScanTarget::Root(root)),
        Err(ScanError::InvalidTarget(_))
    ));
    assert!(matches!(
        scanner.resolve_target(ScanTarget::AllEnabled),
        Err(ScanError::NoMusicFolders)
    ));
}

#[test]
fn asd_02_failed_discovery_preserves_unproven_absence() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for boundary in ["scan.entry.classify", "scan.track.stat", "scan.track.open"] {
        let library = Library::new();
        let root = library.root("music");
        let base = library.path.join("music/A");
        for name in ["keep", "failed", "removed"] {
            wave(&base.join(format!("{name}.wav")), name, 1);
        }
        let scanner = library.scanner();
        scanner
            .scan(ScanTarget::Root(root), ScanMode::Full, None)
            .expect("index");
        let old = library
            .songs()
            .into_iter()
            .find(|song| song.title == "failed")
            .expect("old failed track");
        std::fs::remove_file(base.join("removed.wav")).expect("remove track");
        let failed_path = base.join("failed.wav");
        let (result, events) = observe_with(
            move |event| {
                if event.name == boundary
                    && event.fields.get("path") == Some(&failed_path.to_string_lossy().into_owned())
                {
                    if boundary == "scan.track.open" {
                        std::fs::write(&failed_path, b"broken audio")
                            .expect("inject metadata failure");
                    } else {
                        std::fs::remove_file(&failed_path)
                            .expect("inject classification/stat failure");
                    }
                }
            },
            || {
                scanner
                    .scan(directory(root, "A"), ScanMode::Full, None)
                    .expect("partial discovery")
            },
        );
        assert!(result.tracks_failed > 0, "{boundary}: {events:?}");
        assert!(events.iter().any(|event| {
            event
                .fields
                .get("message")
                .is_some_and(|message| message.contains("failed"))
        }));
        let after = library.songs();
        assert_eq!(
            format!(
                "{:?}",
                after
                    .iter()
                    .find(|song| song.id == old.id)
                    .expect("failed song retained")
            ),
            format!("{old:?}")
        );
        assert_eq!(
            after.iter().any(|song| song.title == "removed"),
            boundary == "scan.entry.classify"
        );
    }
}

#[test]
fn asd_02_failed_descendant_and_moves() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("music");
    let base = library.path.join("music");
    wave(&base.join("A/child/track.wav"), "Track", 1);
    let scanner = library.scanner();
    scanner
        .scan(ScanTarget::Root(root), ScanMode::Full, None)
        .expect("index");
    let before = format!("{:?}", library.songs());
    let inaccessible = base.join("A/child");
    let hidden = library.path.join("hidden");
    let (result, _) = observe_with(
        move |event| {
            if event.name == "scan.directory.enumerate"
                && event.fields.get("path") == Some(&inaccessible.to_string_lossy().into_owned())
            {
                std::fs::rename(&inaccessible, &hidden).expect("inject descendant listing failure");
            }
        },
        || {
            scanner
                .scan(ScanTarget::Root(root), ScanMode::Incremental, None)
                .expect("failed descendant")
        },
    );
    assert_eq!(result.tracks_failed, 1);
    assert_eq!(format!("{:?}", library.songs()), before);
    std::fs::rename(library.path.join("hidden"), base.join("A/child")).expect("restore descendant");
    std::fs::create_dir_all(base.join("destination")).expect("destination");
    std::fs::rename(
        base.join("A/child/track.wav"),
        base.join("destination/track.wav"),
    )
    .expect("move");
    scanner
        .scan(directory(root, "destination"), ScanMode::Incremental, None)
        .expect("destination only");
    assert_eq!(library.songs().len(), 2);
    scanner
        .scan(directory(root, "A/child"), ScanMode::Incremental, None)
        .expect("source only");
    assert_eq!(library.songs().len(), 1);
    scanner
        .scan(ScanTarget::Root(root), ScanMode::Incremental, None)
        .expect("prune confirmed ancestors");
    assert!(
        MusicFolderRepository::new(library.pool.clone())
            .find(root, "A")
            .expect("query")
            .is_none()
    );
}

impl Drop for Library {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).expect("remove fixture directory");
    }
}

#[test]
fn asd_03_registration_and_root_administration() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let root = library.root("registered");
    let folders = MusicFolderRepository::new(library.pool.clone());
    folders.set_enabled(root, false).expect("disable root");
    let child = folders.ensure(root, "nested/child").expect("ancestors");
    std::fs::create_dir_all(&child.path).expect("child directory");
    let alias = library.path.join("alias");
    std::os::unix::fs::symlink(library.path.join("registered"), &alias).expect("root alias");
    for path in [alias, library.path.clone(), PathBuf::from(&child.path)] {
        assert!(
            folders
                .create(&NewMusicFolder {
                    name: "overlap".into(),
                    path: path.to_string_lossy().into_owned(),
                    enabled: true,
                })
                .is_err()
        );
    }
    assert_eq!(folders.find_all().expect("all roots").len(), 1);
    assert!(folders.find_enabled().expect("enabled roots").is_empty());
    assert!(
        !folders
            .set_enabled(child.id, true)
            .expect("generated enable rejected")
    );
    assert!(!folders.delete(child.id).expect("generated remove rejected"));
    assert!(
        folders
            .find_by_id(child.id)
            .expect("generated retained")
            .is_some()
    );
    assert_eq!(folders.find_by_root(child.id).expect("not a root").len(), 0);
    assert_eq!(folders.find_by_root(root).expect("subtree").len(), 3);
}

#[test]
fn asd_02_directory_ownership_and_pruning() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let first = library.root("first");
    let second = library.root("second");
    let directories = MusicFolderRepository::new(library.pool.clone());
    let owner = directories.ensure(first, "Album/./CD1//").expect("owner");
    assert_eq!(
        owner.path,
        library.path.join("first/Album/CD1").to_string_lossy()
    );
    assert_eq!(
        directories
            .ensure(first, "Album/CD1")
            .expect("same owner")
            .id,
        owner.id
    );
    let parent = directories
        .find(first, "Album")
        .expect("query")
        .expect("ancestor");
    assert_eq!(owner.parent, Some(parent.id));
    let root = directories.find(first, "").expect("query").expect("root");
    assert_eq!(parent.parent, Some(root.id));
    assert_eq!(root.parent, None);
    assert!(directories.ensure(first, "../outside").is_err());
    let other = directories
        .ensure(second, "Album/CD1")
        .expect("other root owner");

    let insert = "INSERT INTO songs (id, title, music_folder_id, path, parent_path, content_type, suffix) VALUES (1, 'Track', ?, 'first/track.wav', '', 'audio/wav', 'wav')";
    diesel::sql_query(insert)
        .bind::<diesel::sql_types::Integer, _>(owner.id)
        .execute(&mut library.pool.get().expect("connection"))
        .expect("owned song");
    assert!(
        !directories
            .set_enabled(owner.id, false)
            .expect("generated admin rejected")
    );
    assert!(
        !directories
            .delete(owner.id)
            .expect("generated remove rejected")
    );
    let songs = SongRepository::new(library.pool.clone());
    assert_eq!(
        songs.find_by_directory(owner.id).expect("owned read").len(),
        1
    );
    assert!(
        songs
            .find_by_directory(other.id)
            .expect("other read")
            .is_empty()
    );
    directories.prune(owner.id).expect("prune occupied tree");
    assert!(directories.find(first, "Album").expect("query").is_some());
    diesel::sql_query("DELETE FROM songs WHERE id = 1")
        .execute(&mut library.pool.get().expect("connection"))
        .expect("delete song");
    directories.prune(owner.id).expect("prune empty tree");
    directories.prune(parent.id).expect("prune empty ancestor");
    assert_eq!(directories.find_by_root(first).expect("nodes").len(), 1);
    assert_eq!(
        directories
            .find(first, "")
            .expect("query")
            .expect("root")
            .id,
        root.id
    );
    initialize_database(&mut library.pool.get().expect("connection"))
        .expect("repeat initialization");
}

async fn assert_album_list_filters(app: &axum::Router, root: i32, generated: i32) {
    for kind in [
        "alphabeticalByName",
        "alphabeticalByArtist",
        "newest",
        "frequent",
        "recent",
        "byYear",
        "byGenre",
        "starred",
        "highest",
        "random",
    ] {
        let offset = i32::from(matches!(kind, "starred" | "highest"));
        let response = api(
            app,
            "getAlbumList2",
            &format!("musicFolderId={root}&type={kind}&size=1&offset={offset}&genre=Genre"),
            "admin-key",
        )
        .await;
        let albums = response["albumList2"]["album"]
            .as_array()
            .expect("album list");
        assert_eq!(albums.len(), 1, "{kind}: {response}");
        let name = albums[0]["name"].as_str().expect("album name");
        assert!(matches!(name, "Z1" | "Z2"), "{kind}: {response}");
        if offset > 0 {
            assert_eq!(name, "Z2", "{kind}");
        }
        let invalid = api(
            app,
            "getAlbumList2",
            &format!("musicFolderId={generated}&type={kind}&genre=Genre"),
            "admin-key",
        )
        .await;
        assert_eq!(invalid["status"], "ok", "{kind}: {invalid}");
        assert!(
            invalid["albumList2"]["album"]
                .as_array()
                .is_none_or(Vec::is_empty),
            "{kind}: {invalid}"
        );
    }
}

#[test]
fn asd_03_prefixed_album_details_remain_global() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let library = Library::new();
    let first = library.root("first");
    library.root("second");
    wave(&library.path.join("first/A/one.wav"), "First", 1);
    wave(&library.path.join("second/B/two.wav"), "Second", 2);
    library
        .scanner()
        .scan(ScanTarget::AllEnabled, ScanMode::Full, None)
        .expect("shared index");
    let album = library.songs()[0].album_id.expect("shared album");
    let (app, _) = api_app(&library);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let response = api(
                &app,
                "getMusicDirectory",
                &format!("id=al-{album}&musicFolderId={first}"),
                "admin-key",
            )
            .await;
            assert_eq!(
                response["directory"]["child"]
                    .as_array()
                    .expect("global members")
                    .len(),
                2
            );
            let response = api(
                &app,
                "getAlbum",
                &format!("id=al-{album}&musicFolderId={first}"),
                "admin-key",
            )
            .await;
            assert_eq!(response["album"]["songCount"], 2);
            assert_eq!(response["album"]["duration"], 3);
        });
}

#[test]
fn asd_03_cli_dispatch_in_both_modes() {
    let _test_lock = SCANNER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut library = Library::new();
    let database = library.path.join("cli.db");
    library.pool = DbConfig {
        database_url: database.to_string_lossy().into_owned(),
        max_connections: 1,
        ..DbConfig::default()
    }
    .build_pool()
    .expect("file pool");
    initialize_database(&mut library.pool.get().expect("connection")).expect("file schema");
    let root = library.root("music");
    library.root("other");
    for name in [
        "music/root",
        "music/A/one",
        "music/A/child/two",
        "music/B/three",
        "other/four",
    ] {
        wave(&library.path.join(format!("{name}.wav")), name, 1);
    }
    let run = |args: &[String]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_suboxide"))
            .arg("--database")
            .arg(&database)
            .arg("scan")
            .args(args)
            .env("SUBOXIDE_DATA_DIR", &library.path)
            .output()
            .expect("CLI process");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("CLI output")
    };
    run(&[]);
    for full in [false, true] {
        for (mut args, total) in [
            (vec![], 5),
            (vec!["--folder".into(), root.to_string()], 4),
            (
                vec![
                    "--folder".into(),
                    root.to_string(),
                    "--directory".into(),
                    "A//./".into(),
                ],
                1,
            ),
        ] {
            if full {
                args.push("--full".into());
            }
            let output = run(&args);
            assert!(
                output.contains(&format!("Tracks found:     {total}")),
                "{output}"
            );
            let (updated, skipped) = if full { (total, 0) } else { (0, total) };
            assert!(
                output.contains(&format!("Tracks updated:   {updated}")),
                "{output}"
            );
            assert!(
                output.contains(&format!("Tracks skipped:   {skipped}")),
                "{output}"
            );
        }
    }
    assert_eq!(library.songs().len(), 5);
}
