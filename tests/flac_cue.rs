//! Independent PCM verification of scanner-to-HTTP native CUE delivery.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use diesel::connection::SimpleConnection;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;
use suboxide::app::{AppState, CorsConfig, create_router};
use suboxide::db::{
    DbConfig, DbPool, MusicFolderRepository, NewUser, SongRepository, UserRepository,
    initialize_database,
};
use suboxide::flac::FrameIndex;
use suboxide::models::music::{NewMusicFolder, Song};
use suboxide::scanner::{ScanMode, ScanTarget, Scanner};
use tower::ServiceExt;

const RATE: u32 = 44_101;
const SAMPLE_COUNT: usize = 132_426;
static NEXT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
struct Fixture {
    path: PathBuf,
    pcm: Vec<u8>,
    ticks: [u64; 4],
    source: Vec<u8>,
    index: FrameIndex,
    pool: DbPool,
    root: i32,
    songs: Vec<Song>,
    app: axum::Router,
}

fn ffmpeg(arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("test-only FFmpeg must be available in nix develop");
    let mut stdin = child.stdin.take().expect("input pipe");
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().expect("FFmpeg completed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        writer.join().expect("writer thread").expect("FFmpeg input");
        output.stdout
    })
}

fn decode(bytes: &[u8]) -> Vec<u8> {
    ffmpeg(
        &[
            "-err_detect",
            "crccheck+explode",
            "-i",
            "pipe:0",
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "pipe:1",
        ],
        bytes,
    )
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |mut crc, byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x80 == 0 { 0 } else { 7 };
        }
        crc
    })
}

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |mut crc, byte| {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 == 0 { 0 } else { 0x8005 };
        }
        crc
    })
}

fn variable_image(pcm: &[u8]) -> Vec<u8> {
    let blocks = [
        44_101_u16, 4096, 5000, 9000, 8192, 12_000, 16_000, 20_000, 14_037,
    ];
    let mut bytes = b"fLaC\x80\0\0\x22".to_vec();
    let mut info = [0; 34];
    info[..2].copy_from_slice(&4096_u16.to_be_bytes());
    info[2..4].copy_from_slice(&44_101_u16.to_be_bytes());
    info[10..18].copy_from_slice(
        &((u64::from(RATE) << 44)
            | (1 << 41)
            | (15 << 36)
            | u64::try_from(SAMPLE_COUNT).expect("small fixture"))
        .to_be_bytes(),
    );
    bytes.extend(info);
    let mut start = 0_u32;
    for block in blocks {
        let mut frame = vec![0xff, 0xf9, 0x70, 0x18];
        // These fixture sample numbers are Unicode scalars; standard UTF-8 is
        // an independent encoder for their FLAC coded-number representation.
        frame.extend_from_slice(
            char::from_u32(start)
                .expect("fixture scalar")
                .encode_utf8(&mut [0; 4])
                .as_bytes(),
        );
        frame.extend((block - 1).to_be_bytes());
        frame.push(crc8(&frame));
        for channel in 0..2 {
            frame.push(2); // Verbatim subframe, no wasted bits.
            for sample in usize::try_from(start).expect("small start")
                ..usize::try_from(start).expect("small start") + usize::from(block)
            {
                let offset = sample * 4 + channel * 2;
                frame.extend([pcm[offset + 1], pcm[offset]]);
            }
        }
        frame.extend(crc16(&frame).to_be_bytes());
        bytes.extend(frame);
        start += u32::from(block);
    }
    assert_eq!(usize::try_from(start).expect("small count"), SAMPLE_COUNT);
    bytes
}

fn metadata_blocks(bytes: &[u8]) -> Vec<(u8, &[u8])> {
    assert_eq!(&bytes[..4], b"fLaC");
    let mut offset = 4;
    let mut blocks = Vec::new();
    loop {
        let length = (usize::from(bytes[offset + 1]) << 16)
            | (usize::from(bytes[offset + 2]) << 8)
            | usize::from(bytes[offset + 3]);
        blocks.push((
            bytes[offset] & 0x7f,
            &bytes[offset + 4..offset + 4 + length],
        ));
        if bytes[offset] & 0x80 != 0 {
            return blocks;
        }
        offset += 4 + length;
    }
}

fn add_seektable(path: &Path) {
    let (index, _) = FrameIndex::read(path).expect("source frames");
    let bytes = std::fs::read(path).expect("source bytes");
    if metadata_blocks(&bytes).iter().any(|(kind, _)| *kind == 3) {
        return;
    }
    let mut offset = 4;
    let mut output = bytes.clone();
    loop {
        let length = (usize::from(bytes[offset + 1]) << 16)
            | (usize::from(bytes[offset + 2]) << 8)
            | usize::from(bytes[offset + 3]);
        let last = bytes[offset] & 0x80 != 0;
        output[offset] &= 0x7f;
        offset += 4 + length;
        if last {
            break;
        }
    }
    let mut seek = vec![0x83, 0, 0, 18];
    seek.extend([0; 16]);
    seek.extend(
        u16::try_from(index.frames[0].samples)
            .expect("frame size")
            .to_be_bytes(),
    );
    output.splice(offset..offset, seek);
    std::fs::write(path, output).expect("source seektable");
}

fn source_pcm() -> Vec<u8> {
    (0..SAMPLE_COUNT)
        .flat_map(|sample| {
            let left =
                i16::try_from((i32::try_from(sample % 2048).expect("small sample") - 1024) * 24)
                    .expect("16-bit PCM");
            let right = i16::try_from(
                (i32::try_from(sample * 17 % 8192).expect("small sample") - 4096) * 7,
            )
            .expect("16-bit PCM");
            [left.to_le_bytes(), right.to_le_bytes()].concat()
        })
        .collect()
}

fn write_image(music: &Path, pcm: &[u8], variable: bool) {
    let image = music.join("image.flac");
    if variable {
        std::fs::write(&image, variable_image(pcm)).expect("variable source");
    } else {
        ffmpeg(
            &[
                "-f",
                "s16le",
                "-ar",
                "44101",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-c:a",
                "flac",
                "-compression_level",
                "8",
                "-metadata",
                "title=Image title",
                "-metadata",
                "artist=Image artist",
                "-metadata",
                "album=Image album",
                "-metadata",
                "album_artist=Image album artist",
                "-metadata",
                "genre=Jazz",
                "-metadata",
                "date=1999",
                image.to_str().expect("fixture path"),
            ],
            pcm,
        );
    }
    let cover = image::DynamicImage::new_rgb8(3, 3);
    if variable {
        cover
            .save(music.join("cover.png"))
            .expect("external artwork");
    } else {
        use lofty::{
            config::WriteOptions,
            picture::{MimeType, Picture, PictureType},
            prelude::TagExt,
        };
        let mut png = Cursor::new(Vec::new());
        cover
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG");
        let mut tagged = lofty::read_from_path(&image).expect("source tags");
        let tag = tagged.primary_tag_mut().expect("Vorbis tags");
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Png),
            None,
            png.into_inner(),
        ));
        tag.save_to_path(&image, WriteOptions::default())
            .expect("embedded artwork");
        assert_eq!(
            tagged.primary_tag().expect("tag").title().as_deref(),
            Some("Image title")
        );
    }
    add_seektable(&image);
}

impl Fixture {
    fn new(variable: bool) -> Self {
        let path = std::env::temp_dir().join(format!(
            "suboxide-cue-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let music = path.join("music");
        std::fs::create_dir_all(&music).expect("fixture directory");
        let pcm = source_pcm();
        write_image(&music, &pcm, variable);
        let image = music.join("image.flac");
        let source = std::fs::read(&image).expect("source");
        assert_eq!(decode(&source), pcm);
        let (index, _) = FrameIndex::read(&image).expect("native source index");
        assert_eq!(index.sample_rate, RATE);
        assert_eq!(
            source[usize::try_from(index.frames[0].offset).expect("offset") + 1] & 1,
            u8::from(variable)
        );
        let ticks = if variable {
            [0, 75, 76, 150]
        } else {
            [0, 1, 75, 150]
        };
        let starts = if variable {
            ["00:00:00", "00:01:00", "00:01:01", "00:02:00"]
        } else {
            ["00:00:00", "00:00:01", "00:01:00", "00:02:00"]
        };
        let sheet = format!(
            "TITLE \"CUE Album\"\nPERFORMER \"CUE Artist\"\nFILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nTITLE \"Opening\"\nPERFORMER \"First\"\nINDEX 01 {}\nTRACK 02 AUDIO\nINDEX 01 {}\nTRACK 07 AUDIO\nTITLE \"Guest\"\nPERFORMER \"Guest artist\"\nINDEX 01 {}\nTRACK 09 AUDIO\nINDEX 01 {}\n",
            starts[0], starts[1], starts[2], starts[3]
        );
        std::fs::write(music.join("album.cue"), sheet).expect("sheet");
        let pool = DbConfig {
            database_url: ":memory:".into(),
            max_connections: 1,
            ..DbConfig::default()
        }
        .build_pool()
        .expect("pool");
        initialize_database(&mut pool.get().expect("connection")).expect("schema");
        let hash = suboxide::crypto::hash_password("t").expect("password");
        UserRepository::new(pool.clone())
            .create(&NewUser::admin("t", &hash, "t"))
            .expect("user");
        let root = MusicFolderRepository::new(pool.clone())
            .create(&NewMusicFolder {
                name: "Music".into(),
                path: music.to_string_lossy().into_owned(),
                enabled: true,
            })
            .expect("root")
            .id;
        let scanner = Scanner::with_cover_art_dir(pool.clone(), path.join("covers"));
        let result = scanner
            .scan(ScanTarget::Root(root), ScanMode::Full, None)
            .expect("scan");
        assert_eq!(result.tracks_failed, 0);
        let mut songs = SongRepository::new(pool.clone())
            .find_by_directory(root)
            .expect("songs");
        songs.sort_by_key(|song| song.track_number);
        assert_eq!(songs.len(), 4, "no ordinary image duplicate");
        assert_eq!(
            songs
                .iter()
                .map(|song| song.track_number)
                .collect::<Vec<_>>(),
            [Some(1), Some(2), Some(7), Some(9)]
        );
        let mut conn = pool.get().expect("connection");
        conn.batch_execute("INSERT INTO playlists (id, user_id, name) VALUES (1, 1, 'CUE')")
            .expect("playlist");
        for (position, song) in songs.iter().rev().enumerate() {
            conn.batch_execute(&format!("INSERT INTO playlist_songs (playlist_id, song_id, position) VALUES (1, {}, {position})", song.id)).expect("playlist song");
        }
        drop(conn);
        let app = create_router(
            AppState::new(
                pool.clone(),
                suboxide::lastfm::LastFmClient::new(String::new(), String::new())
                    .expect("disabled Last.fm"),
            ),
            &CorsConfig::default(),
        );
        Self {
            path,
            pcm,
            ticks,
            source,
            index,
            pool,
            root,
            songs,
            app,
        }
    }

    async fn request(&self, endpoint: &str, range: Option<&str>) -> axum::response::Response {
        let mut request =
            Request::builder().uri(format!("/rest/{endpoint}&u=t&p=t&v=1.16.1&c=test&f=json"));
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        self.app
            .clone()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    fn frames(&self, song: &Song) -> std::ops::Range<usize> {
        let snapshot = SongRepository::new(self.pool.clone())
            .audio_source(song.id)
            .expect("snapshot")
            .expect("song");
        let (track, _) = snapshot.cue.expect("CUE identity");
        usize::try_from(track.start_frame).expect("frame start")
            ..usize::try_from(track.end_frame).expect("frame end")
    }

    fn check_audio(&self, song: &Song, bytes: &[u8]) {
        let range = self.frames(song);
        assert_eq!(
            song.file_size,
            i64::try_from(bytes.len()).expect("virtual length")
        );
        let selected = &self.index.frames[range];
        let start = selected[0].sample_start;
        let last = selected.last().expect("selected frame");
        let end = last.sample_start + u64::from(last.samples);
        assert_eq!(
            decode(bytes),
            self.pcm[usize::try_from(start).expect("small start") * 4
                ..usize::try_from(end).expect("small end") * 4]
        );
        assert_eq!(
            u64::try_from(song.duration).expect("duration"),
            (end - start) / u64::from(RATE)
        );
        let inspection = self.path.join("inspection.flac");
        std::fs::write(&inspection, bytes).expect("inspection");
        let (output, _) = FrameIndex::read(&inspection).expect("rebased frames and valid CRCs");
        assert_eq!(output.total_samples, end - start);
        assert_eq!(output.frames[0].sample_start, 0);
        assert_eq!(output.frames.len(), selected.len());
        for (original, emitted) in selected.iter().zip(&output.frames) {
            let payload = |data: &[u8], frame: &suboxide::flac::Frame| {
                let offset = usize::try_from(frame.offset).expect("small offset");
                let end = offset + usize::try_from(frame.length).expect("small length") - 2;
                data[offset + frame.header.len()..end].to_vec()
            };
            assert_eq!(payload(&self.source, original), payload(bytes, emitted));
            assert_eq!(
                bytes[usize::try_from(emitted.offset).expect("offset") + 1] & 1,
                self.source[usize::try_from(original.offset).expect("offset") + 1] & 1
            );
        }
        let blocks = metadata_blocks(bytes);
        assert!(!blocks.iter().any(|(kind, _)| *kind == 3));
        assert_eq!(&blocks[0].1[18..34], &[0; 16]);
        let tags = lofty::read_from_path(&inspection).expect("output tags");
        let tag = tags.primary_tag().expect("Vorbis metadata");
        assert_eq!(tag.title().as_deref(), Some(song.title.as_str()));
        assert_eq!(tag.artist().as_deref(), song.artist_name.as_deref());
        assert_eq!(tag.album().as_deref(), Some("CUE Album"));
        assert_eq!(
            tag.get_string(&lofty::tag::ItemKey::AlbumArtist),
            Some("CUE Artist")
        );
        assert_eq!(
            tag.track(),
            song.track_number
                .map(|number| u32::try_from(number).expect("track"))
        );
        assert_eq!(tags.properties().sample_rate(), Some(RATE));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).expect("fixture cleanup");
    }
}

async fn body(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), 4 << 20)
        .await
        .expect("body")
        .to_vec()
}

#[tokio::test]
async fn fixed_and_variable_images_deliver_exact_whole_frame_pcm_everywhere() {
    for variable in [false, true] {
        let fixture = Fixture::new(variable);
        let songs = &fixture.songs;
        assert_eq!(songs[0].title, "Opening");
        assert_eq!(songs[0].artist_name.as_deref(), Some("First"));
        assert_eq!(
            songs[1].title,
            if variable { "image" } else { "Image title" }
        );
        assert_eq!(songs[1].artist_name.as_deref(), Some("CUE Artist"));
        assert_eq!(songs[2].artist_name.as_deref(), Some("Guest artist"));
        assert!(songs.iter().all(|song| song.album_id == songs[0].album_id));
        assert!(
            songs
                .iter()
                .all(|song| song.cover_art.as_ref().is_some_and(|cover| fixture
                    .path
                    .join("covers")
                    .join(format!("{cover}.png"))
                    .exists()))
        );
        if !variable {
            assert_eq!(songs[1].genre.as_deref(), Some("Jazz"));
            assert_eq!(songs[1].year, Some(1999));
        }
        let ranges: Vec<_> = songs.iter().map(|song| fixture.frames(song)).collect();
        for (range, tick) in ranges.iter().zip(fixture.ticks) {
            let frame = &fixture.index.frames[range.start];
            assert!(frame.sample_start * 75 >= tick * u64::from(RATE));
            if range.start > 0 {
                assert!(
                    fixture.index.frames[range.start - 1].sample_start * 75
                        < tick * u64::from(RATE)
                );
            }
        }
        assert!(ranges.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert_eq!(
            ranges.last().expect("last range").end,
            fixture.index.frames.len()
        );
        for mode in [ScanMode::Incremental, ScanMode::Full] {
            let result =
                Scanner::with_cover_art_dir(fixture.pool.clone(), fixture.path.join("covers"))
                    .scan(ScanTarget::Root(fixture.root), mode, None)
                    .expect("rescan");
            assert_eq!(result.tracks_failed, 0);
            let mut current = SongRepository::new(fixture.pool.clone())
                .find_by_directory(fixture.root)
                .expect("rows");
            current.sort_by_key(|song| song.track_number);
            assert_eq!(
                current.iter().map(|song| song.id).collect::<Vec<_>>(),
                songs.iter().map(|song| song.id).collect::<Vec<_>>()
            );
        }
        check_delivery(&fixture).await;
        let response = fixture
            .request(
                &format!("getAlbum?id=al-{}", songs[0].album_id.expect("album")),
                None,
            )
            .await;
        let json: serde_json::Value =
            serde_json::from_slice(&body(response).await).expect("album JSON");
        let album = &json["subsonic-response"]["album"];
        assert_eq!(album["songCount"], 4);
        assert_eq!(
            album["duration"],
            songs.iter().map(|song| song.duration).sum::<i32>()
        );
        assert_eq!(album["artist"], "CUE Artist");
        assert!(album["coverArt"].is_string());
        let audio_count = std::fs::read_dir(fixture.path.join("music"))
            .expect("music files")
            .map(|entry| entry.expect("directory entry"))
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "flac"))
            .count();
        assert_eq!(audio_count, 1, "runtime generated no audio files");
    }
}

async fn check_delivery(fixture: &Fixture) {
    let songs = &fixture.songs;
    let mut expected = Vec::new();
    for song in songs {
        let response = fixture
            .request(&format!("stream?id=mf-{}", song.id), None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let length = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .expect("length")
            .parse::<usize>()
            .expect("number");
        let bytes = body(response).await;
        assert_eq!(bytes.len(), length);
        fixture.check_audio(song, &bytes);
        let download = fixture
            .request(&format!("download?id=mf-{}", song.id), None)
            .await;
        assert!(
            download.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .expect("filename")
                .contains(&format!("{:02} -", song.track_number.expect("number")))
        );
        let downloaded = body(download).await;
        assert_eq!(downloaded, bytes);
        assert_eq!(decode(&downloaded), decode(&bytes));
        for (start, end) in [
            (0, 7),
            (35, 75),
            (length / 2, length / 2 + 111),
            (length - 9, length - 1),
        ] {
            let response = fixture
                .request(
                    &format!("stream?id=mf-{}", song.id),
                    Some(&format!("bytes={start}-{end}")),
                )
                .await;
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                format!("bytes {start}-{end}/{length}")
            );
            assert_eq!(body(response).await, bytes[start..=end]);
        }
        expected.push((song, bytes));
    }
    let mut artists: Vec<_> = songs.iter().filter_map(|song| song.artist_id).collect();
    artists.sort_unstable();
    artists.dedup();
    let mut archives = vec![
        format!("al-{}", songs[0].album_id.expect("album")),
        "pl-1".into(),
    ];
    archives.extend(artists.into_iter().map(|artist| format!("ar-{artist}")));
    for id in archives {
        let response = fixture.request(&format!("download?id={id}"), None).await;
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
        let mut zip = zip::ZipArchive::new(Cursor::new(body(response).await)).expect("ZIP");
        let mut names = std::collections::HashSet::new();
        let mut count = 0;
        for position in 0..zip.len() {
            let mut entry = zip.by_index(position).expect("entry");
            if !Path::new(entry.name())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("flac"))
            {
                continue;
            }
            assert!(
                names.insert(entry.name().to_owned()),
                "distinct track filenames"
            );
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("audio");
            let (song, _) = expected
                .iter()
                .find(|(_, full)| *full == bytes)
                .expect("same virtual stream");
            assert!(
                entry
                    .name()
                    .contains(&format!("{:02} -", song.track_number.expect("number")))
            );
            fixture.check_audio(song, &bytes);
            count += 1;
        }
        if id.starts_with("ar-") {
            assert!(count > 0);
        } else {
            assert_eq!(count, 4);
        }
    }
}

fn runtime_command(fixture: &Fixture) -> Command {
    let binary = std::env::var_os("SUBOXIDE_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_suboxide").into());
    let mut command = Command::new(binary);
    command
        .current_dir(&fixture.path)
        .env("PATH", "")
        .env("SUBOXIDE_DATA_DIR", fixture.path.join("runtime-data"))
        .env_remove("LASTFM_API_KEY")
        .env_remove("LASTFM_API_SECRET")
        .arg("--database")
        .arg(fixture.path.join("runtime.db"));
    command
}

fn runtime_cli(fixture: &Fixture, arguments: &[&str]) {
    let output = runtime_command(fixture)
        .args(arguments)
        .output()
        .expect("runtime binary");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Debug)]
struct Server(std::process::Child);

impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().expect("server status").is_none() {
            self.0.kill().expect("stop test server");
        }
        self.0.wait().expect("reap test server");
    }
}

async fn start_server(fixture: &Fixture) -> (Server, reqwest::Client, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let mut server = Server(
        runtime_command(fixture)
            .args(["--port", &port.to_string(), "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("runtime server"),
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("HTTP client");
    let base = format!("http://127.0.0.1:{port}/rest");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            server.0.try_wait().expect("server running").is_none(),
            "server exited"
        );
        if client
            .get(format!("{base}/ping?u=t&p=t&v=1.16.1&c=test&f=json"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "server startup timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    (server, client, base)
}

#[tokio::test]
async fn runtime_binary_scans_and_delivers_with_empty_path() {
    let fixture = Fixture::new(false);
    runtime_cli(
        &fixture,
        &[
            "user",
            "create",
            "--username",
            "t",
            "--password",
            "t",
            "--admin",
        ],
    );
    runtime_cli(
        &fixture,
        &[
            "folder",
            "add",
            "--name",
            "Music",
            "--path",
            fixture.path.join("music").to_str().expect("music path"),
        ],
    );
    runtime_cli(&fixture, &["scan", "--full"]);
    runtime_cli(&fixture, &["scan"]);
    let pool = DbConfig {
        database_url: fixture
            .path
            .join("runtime.db")
            .to_string_lossy()
            .into_owned(),
        ..DbConfig::default()
    }
    .build_pool()
    .expect("runtime database");
    let mut songs = SongRepository::new(pool)
        .find_by_directory(1)
        .expect("runtime songs");
    songs.sort_by_key(|song| song.track_number);
    assert_eq!(songs.len(), 4);
    assert!(songs.iter().all(|song| song.cue_track_id.is_some()));
    let (server, client, base) = start_server(&fixture).await;
    let mut full = Vec::new();
    for song in &songs {
        for endpoint in ["stream", "download"] {
            let response = client
                .get(format!(
                    "{base}/{endpoint}?id=mf-{}&u=t&p=t&v=1.16.1&c=test&f=json",
                    song.id
                ))
                .send()
                .await
                .expect("runtime response");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.bytes().await.expect("runtime audio").to_vec();
            fixture.check_audio(song, &bytes);
            if endpoint == "stream" {
                full.push(bytes);
            }
        }
    }
    let response = client
        .get(format!(
            "{base}/download?id=al-{}&u=t&p=t&v=1.16.1&c=test&f=json",
            songs[0].album_id.expect("album")
        ))
        .send()
        .await
        .expect("runtime ZIP");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    let mut archive = zip::ZipArchive::new(Cursor::new(response.bytes().await.expect("archive")))
        .expect("runtime ZIP");
    let mut count = 0;
    for position in 0..archive.len() {
        let mut entry = archive.by_index(position).expect("entry");
        if !Path::new(entry.name())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("flac"))
        {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("ZIP audio");
        assert!(full.contains(&bytes));
        assert!(!decode(&bytes).is_empty());
        count += 1;
    }
    assert_eq!(count, 4);
    drop(server);
    assert_eq!(
        std::fs::read_dir(fixture.path.join("music"))
            .expect("source directory")
            .count(),
        2,
        "only image and CUE; no generated audio"
    );
}

#[tokio::test]
async fn media_parses_only_the_selected_interval_and_rejects_invalid_frames_before_ranges() {
    for variable in [false, true] {
        let fixture = Fixture::new(variable);
        let song = fixture.songs.last().expect("last track");
        let path = Path::new(&song.path);
        let modified = std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let repo = SongRepository::new(fixture.pool.clone());
        let snapshot = repo.audio_source(song.id).expect("snapshot").expect("song");
        let (track, source) = snapshot.cue.expect("typed source");
        let expected = body(
            fixture
                .request(&format!("stream?id={}", song.id), None)
                .await,
        )
        .await;
        assert_eq!(song.file_size, i64::try_from(expected.len()).expect("size"));
        let positions = [
            fixture.index.frames[0].offset + fixture.index.frames[0].header.len() as u64 + 1,
            u64::try_from(track.byte_start).expect("start") + 1,
        ];
        for (probe, offset) in positions.into_iter().enumerate() {
            let mut bytes = fixture.source.clone();
            bytes[usize::try_from(offset).expect("offset")] ^= 1;
            std::fs::write(path, bytes).expect("equal-length corruption");
            restore_mtime(path, modified);
            assert_eq!(
                suboxide::flac::Fingerprint::from_metadata(
                    &std::fs::metadata(path).expect("metadata")
                )
                .expect("fingerprint"),
                source.fingerprint().expect("stored fingerprint")
            );
            for endpoint in ["stream", "download"] {
                let response = fixture
                    .request(&format!("{endpoint}?id={}", song.id), None)
                    .await;
                let bytes = body(response).await;
                if probe == 0 {
                    assert_eq!(bytes, expected, "preceding audio is never parsed");
                } else {
                    assert!(
                        !bytes.starts_with(b"fLaC"),
                        "bad selected frame is rejected"
                    );
                }
            }
            let response = fixture
                .request(&format!("stream?id={}", song.id), Some("bytes=0-15"))
                .await;
            if probe == 0 {
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert_eq!(body(response).await, expected[..16]);
            } else {
                assert_ne!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert!(!body(response).await.starts_with(b"fLaC"));
            }
        }
        std::fs::write(path, &fixture.source).expect("restore source");
        restore_mtime(path, modified);
        for mutation in [
            format!(
                "UPDATE cue_tracks SET end_frame = {} WHERE id = {}",
                track.end_frame + 1,
                track.id
            ),
            format!(
                "UPDATE cue_tracks SET byte_start = {} WHERE id = {}",
                track.byte_start + 1,
                track.id
            ),
            format!(
                "UPDATE cue_tracks SET byte_end = {} WHERE id = {}",
                track.byte_end - 1,
                track.id
            ),
        ] {
            fixture
                .pool
                .get()
                .expect("connection")
                .batch_execute(&mutation)
                .expect("inconsistent stored interval");
            let response = fixture
                .request(&format!("stream?id={}", song.id), Some("bytes=0-15"))
                .await;
            assert_ne!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert!(!body(response).await.starts_with(b"fLaC"));
            fixture.pool.get().expect("connection").batch_execute(&format!("UPDATE cue_tracks SET end_frame = {}, byte_start = {}, byte_end = {} WHERE id = {}", track.end_frame, track.byte_start, track.byte_end, track.id)).expect("restore boundaries");
        }
    }
}

fn restore_mtime(path: &Path, modified: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("restore indexed precise mtime");
}
