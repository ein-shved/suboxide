//! Subsonic API compatible server.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use suboxide::app::{AppState, CorsConfig, CorsConfigError, create_router};
use suboxide::crypto::{PasswordError, hash_password};
use suboxide::db::{
    DbConfig, DbPool, MusicFolderRepository, MusicRepoError, NewUser, SETTING_LASTFM_API_KEY,
    SETTING_LASTFM_API_SECRET, SettingsRepository, UserRepoError, UserRepository, UserUpdate,
    initialize_database,
};
use suboxide::lastfm::{LastFmClient, LastFmError};
use suboxide::models::music::NewMusicFolder;
use suboxide::scanner::state::ScanSnapshot;
use suboxide::scanner::{
    AutoScanner, ScanError, ScanMode, ScanState, ScanStateHandle, ScanTarget, Scanner,
};

/// Subsonic-compatible music streaming server.
#[derive(Parser)]
#[command(name = "suboxide")]
#[command(about = "A Subsonic API compatible music server written in Rust")]
struct Cli {
    /// Database file path
    #[arg(short, long, default_value = "suboxide.db")]
    database: PathBuf,

    /// Server port
    #[arg(short, long, default_value = "4040")]
    port: u16,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage users
    #[command(subcommand)]
    User(UserCommands),

    /// Manage API keys
    #[command(subcommand)]
    ApiKey(ApiKeyCommands),

    /// Manage music folders
    #[command(subcommand)]
    Folder(FolderCommands),

    /// Manage Last.fm integration
    #[command(subcommand)]
    Lastfm(LastfmCommands),

    /// Scan music folders for audio files
    Scan {
        /// Specific folder ID to scan (scans all if not specified)
        #[arg(short, long)]
        folder: Option<i32>,

        /// Update only the direct entries of a root-relative directory
        #[arg(long, requires = "folder")]
        directory: Option<PathBuf>,

        /// Run full scan (re-scan all files regardless of modification time)
        #[arg(long)]
        full: bool,
    },

    /// Start the server (default)
    Serve {
        /// Enable auto-scan (periodic incremental scanning)
        #[arg(long)]
        auto_scan: bool,

        /// Auto-scan interval in seconds (default: 300 = 5 minutes)
        #[arg(long, default_value = "300")]
        auto_scan_interval: u64,
    },
}

#[derive(Subcommand)]
enum UserCommands {
    /// Create a new user
    Create {
        /// Username
        #[arg(short, long)]
        username: String,

        /// Password
        #[arg(short, long)]
        password: String,

        /// Create as admin user
        #[arg(short, long)]
        admin: bool,
    },

    /// List all users
    List,

    /// Update a user
    Update {
        /// Username of the user to update
        #[arg(short, long)]
        username: String,

        /// New password
        #[arg(short, long)]
        password: Option<String>,

        /// Set admin role
        #[arg(long)]
        admin: Option<bool>,

        /// Set email
        #[arg(long)]
        email: Option<String>,
    },

    /// Delete a user
    Delete {
        /// Username of the user to delete
        #[arg(short, long)]
        username: String,
    },
}

#[derive(Subcommand)]
enum ApiKeyCommands {
    /// Generate an API key for a user
    Generate {
        /// Username of the user
        #[arg(short, long)]
        username: String,
    },

    /// Revoke (delete) an API key for a user
    Revoke {
        /// Username of the user
        #[arg(short, long)]
        username: String,
    },

    /// Show a user's API key
    Show {
        /// Username of the user
        #[arg(short, long)]
        username: String,
    },
}

#[derive(Subcommand)]
enum FolderCommands {
    /// Add a music folder
    Add {
        /// Name of the music folder
        #[arg(short, long)]
        name: String,

        /// Path to the music folder
        #[arg(short, long)]
        path: PathBuf,
    },

    /// List all music folders
    List,

    /// Remove a music folder
    Remove {
        /// ID of the folder to remove
        #[arg(short, long)]
        id: i32,
    },
}

#[derive(Subcommand)]
enum LastfmCommands {
    /// Configure Last.fm API credentials (stored in the database)
    Configure {
        /// Last.fm API key (prompted if omitted)
        #[arg(long)]
        api_key: Option<String>,

        /// Last.fm API secret (prompted if omitted)
        #[arg(long)]
        api_secret: Option<String>,
    },

    /// Set a user's Last.fm session key manually
    Set {
        /// Username of the user
        #[arg(short, long)]
        username: String,

        /// Last.fm session key
        #[arg(short, long)]
        session_key: String,
    },

    /// Unlink (clear) a user's Last.fm session key
    Unlink {
        /// Username of the user
        #[arg(short, long)]
        username: String,
    },

    /// Interactively link a user's Last.fm account
    Link {
        /// Username of the user
        #[arg(short, long)]
        username: String,

        /// Token from Last.fm (optional, to bypass interactive prompt)
        #[arg(long)]
        token: Option<String>,
    },

    /// Debug Last.fm artist lookup
    Debug {
        /// Artist name to look up
        #[arg(short, long)]
        artist: String,
    },
}

/// Errors that can occur during database setup.
#[derive(Debug, thiserror::Error)]
enum SetupError {
    #[error("Failed to create database pool: {0}")]
    PoolCreation(#[source] suboxide::db::DbPoolError),
    #[error("Failed to connect: {0}")]
    Connection(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Invalid database path")]
    InvalidPath,
    #[error("Failed to initialize database: {0}")]
    Initialization(#[source] diesel::result::Error),
}

fn setup_database(database_path: impl AsRef<std::path::Path>) -> Result<DbPool, SetupError> {
    let database_url = database_path
        .as_ref()
        .to_str()
        .ok_or(SetupError::InvalidPath)?;
    let config = DbConfig::new(database_url);
    let pool = config.build_pool().map_err(SetupError::PoolCreation)?;

    let mut conn = pool
        .get()
        .map_err(|e| SetupError::Connection(Box::new(e)))?;
    initialize_database(&mut conn).map_err(SetupError::Initialization)?;

    Ok(pool)
}

fn create_user(
    pool: &DbPool,
    username: &str,
    password: &str,
    admin: bool,
) -> Result<(), UserCommandError> {
    let password_hash = hash_password(password)?;
    let repo = UserRepository::new(pool.clone());

    let new_user = if admin {
        NewUser::admin(username, &password_hash, password)
    } else {
        NewUser::regular(username, &password_hash, password)
    };

    match repo.create(&new_user) {
        Ok(user) => {
            println!(
                "Created user '{}' (id: {}, admin: {})",
                user.username, user.id, admin
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, thiserror::Error)]
enum UserCommandError {
    #[error("User '{0}' not found")]
    NotFound(String),
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    Repository(#[from] UserRepoError),
}

fn run_user_command(pool: &DbPool, cmd: UserCommands) -> Result<(), UserCommandError> {
    match cmd {
        UserCommands::Create {
            username,
            password,
            admin,
        } => {
            create_user(pool, &username, &password, admin)?;
        }
        UserCommands::List => {
            let users = UserRepository::new(pool.clone()).find_all()?;
            if users.is_empty() {
                println!("No users found.");
            } else {
                println!("Users:");
                for user in users {
                    let roles: Vec<&str> = [
                        (user.roles.admin_role, "admin"),
                        (user.roles.settings_role, "settings"),
                        (user.roles.stream_role, "stream"),
                        (user.roles.download_role, "download"),
                        (user.roles.upload_role, "upload"),
                        (user.roles.jukebox_role, "jukebox"),
                        (user.roles.playlist_role, "playlist"),
                        (user.roles.cover_art_role, "cover_art"),
                        (user.roles.comment_role, "comment"),
                        (user.roles.podcast_role, "podcast"),
                        (user.roles.share_role, "share"),
                        (user.roles.video_conversion_role, "video"),
                    ]
                    .iter()
                    .filter(|(enabled, _)| *enabled)
                    .map(|(_, name)| *name)
                    .collect();

                    println!(
                        "  [{}] {} (roles: {})",
                        user.id,
                        user.username,
                        roles.join(", ")
                    );
                }
            }
        }
        UserCommands::Update {
            username,
            password,
            admin,
            email,
        } => {
            let repo = UserRepository::new(pool.clone());
            let mut builder = UserUpdate::builder(&username);

            if let Some(email) = email {
                builder = builder.email(email);
            }
            if let Some(admin) = admin {
                builder = builder.admin_role(admin);
            }

            let update = builder.build();

            let user_id = match repo.find_by_username(&username)? {
                Some(user) => user.id,
                None => return Err(UserCommandError::NotFound(username.clone())),
            };

            if repo.update_user(&update)? {
                println!("Updated user details for '{username}'");
            } else {
                return Err(UserCommandError::NotFound(username));
            }

            if let Some(password) = password {
                let hash = hash_password(&password)?;
                if !repo.update_password(user_id, &hash)? {
                    return Err(UserCommandError::NotFound(username));
                }
                println!("Updated password for '{username}'");
            }
        }
        UserCommands::Delete { username } => {
            let repo = UserRepository::new(pool.clone());
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(UserCommandError::NotFound(username));
            };
            if repo.delete(user.id)? {
                println!("Deleted user '{username}'");
            } else {
                return Err(UserCommandError::NotFound(username));
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum ApiKeyCommandError {
    #[error("User '{0}' not found")]
    NotFound(String),
    #[error(transparent)]
    Repository(#[from] UserRepoError),
}

fn run_api_key_command(pool: &DbPool, cmd: ApiKeyCommands) -> Result<(), ApiKeyCommandError> {
    let repo = UserRepository::new(pool.clone());
    match cmd {
        ApiKeyCommands::Generate { username } => {
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(ApiKeyCommandError::NotFound(username));
            };
            let api_key = repo.generate_api_key(user.id)?;
            println!("Generated API key for user '{}':", user.username);
            println!("{api_key}");
        }
        ApiKeyCommands::Revoke { username } => {
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(ApiKeyCommandError::NotFound(username));
            };
            if repo.revoke_api_key(user.id)? {
                println!("Revoked API key for user '{username}'");
            } else {
                return Err(ApiKeyCommandError::NotFound(username));
            }
        }
        ApiKeyCommands::Show { username } => {
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(ApiKeyCommandError::NotFound(username));
            };
            if let Some(api_key) = user.api_key {
                println!("API key for user '{username}':");
                println!("{api_key}");
            } else {
                println!("User '{username}' has no API key. Generate one with:");
                println!("  suboxide api-key generate --username {username}");
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum FolderCommandError {
    #[error("Music folder not found")]
    NotFound,
    #[error(transparent)]
    Repository(#[from] MusicRepoError),
}

fn run_folder_command(pool: &DbPool, cmd: FolderCommands) -> Result<(), FolderCommandError> {
    let repo = MusicFolderRepository::new(pool.clone());
    match cmd {
        FolderCommands::Add { name, path } => {
            let path_str = path.to_string_lossy().into_owned();
            let new_folder = NewMusicFolder::new(&name, &path_str);
            let folder = repo.create(&new_folder)?;
            println!("Added music folder '{}' (id: {})", folder.name, folder.id);
            println!("  Path: {}", folder.path);
        }
        FolderCommands::List => {
            let folders = repo.find_all()?;
            if folders.is_empty() {
                println!("No music folders configured. Add one with:");
                println!("  suboxide folder add --name \"Music\" --path /path/to/music");
            } else {
                println!("Music folders:");
                for folder in folders {
                    let status = if folder.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    println!(
                        "  [{}] {} - {} ({})",
                        folder.id, folder.name, folder.path, status
                    );
                }
            }
        }
        FolderCommands::Remove { id } => {
            if repo.delete(id)? {
                println!("Removed music folder with id {id}");
            } else {
                return Err(FolderCommandError::NotFound);
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum LastfmCommandError {
    #[error("User '{0}' not found")]
    UserNotFound(String),
    #[error(transparent)]
    UserRepo(#[from] UserRepoError),
    #[error(transparent)]
    LastFm(#[from] LastFmError),
    #[error(transparent)]
    MusicRepo(#[from] MusicRepoError),
    #[error(
        "Last.fm is not configured. Run `suboxide lastfm configure` or set LASTFM_API_KEY and LASTFM_API_SECRET."
    )]
    NotConfigured,
    #[error("Operation cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Read a trimmed line from stdin, `None` when empty.
fn prompt_value(message: &str) -> Result<Option<String>, std::io::Error> {
    println!("{message}");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

fn configure_lastfm_credentials(
    pool: &DbPool,
    api_key: Option<String>,
    api_secret: Option<String>,
) -> Result<(), LastfmCommandError> {
    let settings = SettingsRepository::new(pool.clone());

    let api_key = match api_key {
        Some(key) => key,
        None => prompt_value("Last.fm API key:")?.ok_or(LastfmCommandError::Cancelled)?,
    };
    let api_secret = match api_secret {
        Some(secret) => secret,
        None => prompt_value("Last.fm API secret:")?.ok_or(LastfmCommandError::Cancelled)?,
    };

    settings.set(SETTING_LASTFM_API_KEY, api_key.trim())?;
    settings.set(SETTING_LASTFM_API_SECRET, api_secret.trim())?;
    println!("Last.fm credentials stored. Restart the server to apply.");
    Ok(())
}

async fn run_lastfm_command(pool: &DbPool, cmd: LastfmCommands) -> Result<(), LastfmCommandError> {
    let repo = UserRepository::new(pool.clone());
    match cmd {
        LastfmCommands::Configure {
            api_key,
            api_secret,
        } => {
            configure_lastfm_credentials(pool, api_key, api_secret)?;
        }
        LastfmCommands::Set {
            username,
            session_key,
        } => {
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(LastfmCommandError::UserNotFound(username));
            };
            if repo.set_lastfm_session_key(user.id, Some(&session_key))? {
                println!("Set Last.fm session key for user '{}'", user.username);
            } else {
                return Err(LastfmCommandError::UserNotFound(user.username));
            }
        }
        LastfmCommands::Unlink { username } => {
            let Some(user) = repo.find_by_username(&username)? else {
                return Err(LastfmCommandError::UserNotFound(username));
            };
            if repo.set_lastfm_session_key(user.id, None)? {
                println!("Cleared Last.fm session key for user '{username}'");
            } else {
                return Err(LastfmCommandError::UserNotFound(username));
            }
        }
        LastfmCommands::Link { username, token } => {
            let client = load_lastfm_client(pool)?;
            if !client.is_configured() {
                return Err(LastfmCommandError::NotConfigured);
            }

            let Some(user) = repo.find_by_username(&username)? else {
                return Err(LastfmCommandError::UserNotFound(username.clone()));
            };

            let token = if let Some(t) = token {
                t
            } else {
                println!("To link your Last.fm account, please visit:");
                println!(
                    "http://www.last.fm/api/auth/?api_key={}&cb=http://localhost:8080/callback",
                    client.api_key()?
                );
                println!(
                    "\nAfter approving access, you will be redirected to a URL (or see a token)."
                );

                prompt_value("Please paste the 'token' parameter from the URL here:")?
                    .ok_or(LastfmCommandError::Cancelled)?
            };

            println!("Exchanging token for session...");
            let session = client.get_session(&token).await?;
            println!(
                "Successfully authenticated as Last.fm user: {}",
                session.name
            );
            if !repo.set_lastfm_session_key(user.id, Some(&session.key))? {
                return Err(LastfmCommandError::UserNotFound(user.username));
            }
            println!("Last.fm session linked successfully!");
        }
        LastfmCommands::Debug { artist } => {
            let client = load_lastfm_client(pool)?;
            if !client.is_configured() {
                return Err(LastfmCommandError::NotConfigured);
            }

            println!("Querying Last.fm for artist: '{artist}'");
            match client.get_artist_info(&artist).await? {
                Some(info) => {
                    println!("Found artist: {}", info.name);
                    println!("URL: {:?}", info.url);
                    println!("MBID: {:?}", info.musicbrainz_id);
                    println!("Images:");
                    for img in &info.image {
                        println!("  - [{}] {}", img.size, img.url);
                        if img.url.contains("2a96cbd8b46e442fc41c2b86b821562f") {
                            println!(
                                "    ^ WARNING: This hash is known to be a placeholder star image!"
                            );
                        }
                    }
                }
                None => println!("Artist not found on Last.fm"),
            }
        }
    }
    Ok(())
}

fn run_scan_command(
    pool: &DbPool,
    folder: Option<i32>,
    directory: Option<PathBuf>,
    full: bool,
) -> Result<(), ScanError> {
    let scanner = Scanner::new(pool.clone());
    let mode = if full {
        ScanMode::Full
    } else {
        ScanMode::Incremental
    };
    let target = scanner.resolve_target(match (folder, directory) {
        (Some(root_id), Some(path)) => ScanTarget::Directory { root_id, path },
        (Some(root_id), None) => ScanTarget::Root(root_id),
        (None, None) => ScanTarget::AllEnabled,
        (None, Some(_)) => {
            return Err(ScanError::InvalidTarget(
                "--directory requires --folder".into(),
            ));
        }
    })?;
    let scan_state = ScanStateHandle::new(ScanState::new());
    let progress = CliScanProgress::start(scan_state.clone());

    let result = {
        let _guard = scan_state.try_start().expect("new scan state should start");
        scanner.scan(target, mode, Some(&scan_state))
    };
    progress.finish();

    let stats = result?;
    println!("\nScan complete:");
    println!("  Tracks found:     {}", stats.tracks_found);
    println!("  Tracks added:     {}", stats.tracks_added);
    println!("  Tracks updated:   {}", stats.tracks_updated);
    println!("  Tracks skipped:   {}", stats.tracks_skipped);
    println!("  Tracks removed:   {}", stats.tracks_removed);
    println!("  Tracks failed:    {}", stats.tracks_failed);
    println!("  Artists added:    {}", stats.artists_added);
    println!("  Albums added:     {}", stats.albums_added);
    println!("  Cover art saved:  {}", stats.cover_art_saved);
    Ok(())
}

struct CliScanProgress {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl CliScanProgress {
    fn start(scan_state: ScanStateHandle) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !std::io::stderr().is_terminal() {
            return Self { stop, handle: None };
        }

        let render_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let started_at = Instant::now();
            while !render_stop.load(Ordering::Relaxed) {
                let snapshot = scan_state.snapshot();
                eprint!(
                    "\r{}",
                    format_scan_progress(&snapshot, started_at.elapsed())
                );
                let _ = std::io::stderr().flush();
                thread::sleep(Duration::from_millis(120));
            }
            eprint!("\r{}\r", " ".repeat(96));
            let _ = std::io::stderr().flush();
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn format_scan_progress(snapshot: &ScanSnapshot, elapsed: Duration) -> String {
    let count = snapshot.count;
    let total = snapshot.total;
    let phase = snapshot.phase.as_str();
    let folder = snapshot.current_folder.as_deref().unwrap_or("all folders");
    let elapsed = format_elapsed(elapsed);
    if total > 0 {
        let percent = count
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(0)
            .min(100);
        format!(
            "Scanning [{bar}] {count}/{total} {percent:>3}% elapsed {elapsed} {phase} {folder}",
            bar = progress_bar(count, total, 24),
        )
    } else {
        format!(
            "Scanning [{bar}] elapsed {elapsed} {phase} {folder}",
            bar = spinner_frame(count)
        )
    }
}

fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn progress_bar(count: u64, total: u64, width: usize) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let width_u128 = u128::try_from(width).unwrap_or(u128::MAX);
    let filled_u128 = (u128::from(count.min(total)) * width_u128) / u128::from(total);
    let filled = usize::try_from(filled_u128).unwrap_or(width).min(width);
    format!("{}{}", "#".repeat(filled), "-".repeat(width - filled))
}

const fn spinner_frame(count: u64) -> &'static str {
    match count % 4 {
        0 => "|",
        1 => "/",
        2 => "-",
        _ => "\\",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use suboxide::scanner::ScanPhase;
    use suboxide::scanner::state::ScanSnapshot;

    use super::{
        Cli, Commands, DbConfig, FolderCommandError, FolderCommands, ScanError, format_elapsed,
        format_scan_progress, initialize_database, progress_bar, run_folder_command,
        run_scan_command, spinner_frame,
    };
    use clap::Parser;

    #[test]
    fn progress_bar_fills_proportionally_and_clamps_overflow() {
        assert_eq!(progress_bar(0, 10, 10), "----------");
        assert_eq!(progress_bar(5, 10, 10), "#####-----");
        assert_eq!(progress_bar(15, 10, 10), "##########");
    }

    #[test]
    fn progress_bar_handles_zero_total_and_width() {
        assert_eq!(progress_bar(1, 0, 10), "");
        assert_eq!(progress_bar(1, 10, 0), "");
    }

    #[test]
    fn spinner_frame_cycles_deterministically() {
        assert_eq!(spinner_frame(0), "|");
        assert_eq!(spinner_frame(1), "/");
        assert_eq!(spinner_frame(2), "-");
        assert_eq!(spinner_frame(3), "\\");
        assert_eq!(spinner_frame(4), "|");
    }

    #[test]
    fn format_elapsed_renders_mm_ss() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "01:05");
    }

    #[test]
    fn format_scan_progress_includes_elapsed_time() {
        let snapshot = ScanSnapshot {
            scanning: true,
            count: 5,
            total: 10,
            phase: ScanPhase::Processing,
            current_folder: Some("Library".to_string()),
        };

        let rendered = format_scan_progress(&snapshot, Duration::from_secs(65));

        assert!(rendered.contains("5/10"));
        assert!(rendered.contains(" 50%"));
        assert!(rendered.contains("elapsed 01:05"));
        assert!(rendered.contains("processing Library"));
    }
    #[test]
    fn scan_directory_requires_folder() {
        assert!(Cli::try_parse_from(["suboxide", "scan", "--directory", "A"]).is_err());
        let cli = Cli::try_parse_from([
            "suboxide",
            "scan",
            "--folder",
            "7",
            "--directory",
            "A/./",
            "--full",
        ])
        .expect("valid selector");
        assert!(
            matches!(cli.command, Some(Commands::Scan { folder: Some(7), directory: Some(path), full: true }) if path == Path::new("A/./"))
        );
        assert!(matches!(
            Cli::try_parse_from(["suboxide", "scan"])
                .expect("all roots")
                .command,
            Some(Commands::Scan {
                folder: None,
                directory: None,
                full: false
            })
        ));
    }

    #[test]
    fn scan_command_reports_selection_errors_before_work() {
        let pool = DbConfig {
            database_url: ":memory:".into(),
            max_connections: 1,
            ..DbConfig::default()
        }
        .build_pool()
        .expect("pool");
        initialize_database(&mut pool.get().expect("connection")).expect("schema");
        assert!(matches!(
            run_scan_command(&pool, None, None, false),
            Err(ScanError::NoMusicFolders)
        ));
        assert!(matches!(
            run_scan_command(&pool, Some(999), None, false),
            Err(ScanError::FolderNotFound(_))
        ));
        assert!(matches!(
            run_scan_command(&pool, None, Some("A".into()), false),
            Err(ScanError::InvalidTarget(_))
        ));
        diesel::RunQueryDsl::execute(diesel::sql_query("INSERT INTO music_folders (id, name, path, parent) VALUES (1, 'root', '/root', NULL), (2, 'child', '/root/child', 1)"), &mut pool.get().expect("connection")).expect("folders");
        assert!(matches!(
            run_scan_command(&pool, Some(2), None, true),
            Err(ScanError::FolderNotFound(_))
        ));
        assert!(matches!(
            run_folder_command(&pool, FolderCommands::Remove { id: 2 }),
            Err(FolderCommandError::NotFound)
        ));
    }
}

/// Load the Last.fm client, preferring environment variables and falling
/// back to credentials stored via `suboxide lastfm configure`.
fn load_lastfm_client(pool: &DbPool) -> Result<LastFmClient, LastFmError> {
    let settings = SettingsRepository::new(pool.clone());

    let api_key = std::env::var("LASTFM_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| settings.get(SETTING_LASTFM_API_KEY).ok().flatten())
        .unwrap_or_default();
    let api_secret = std::env::var("LASTFM_API_SECRET")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| settings.get(SETTING_LASTFM_API_SECRET).ok().flatten())
        .unwrap_or_default();

    LastFmClient::new(api_key, api_secret)
}

async fn run_server_command_or_exit(
    pool: DbPool,
    port: u16,
    auto_scan: bool,
    auto_scan_interval: u64,
) {
    let lastfm_client = match load_lastfm_client(&pool) {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e, "Last.fm client initialization failed");
            std::process::exit(1);
        }
    };

    if lastfm_client.is_configured() {
        tracing::info!("Last.fm integration enabled");
    } else {
        tracing::info!(
            "Last.fm not configured; scrobbling disabled. \
             Run `suboxide lastfm configure` or set LASTFM_API_KEY/LASTFM_API_SECRET."
        );
    }

    if let Err(e) = run_server(ServerConfig {
        pool,
        port,
        auto_scan,
        auto_scan_interval,
        lastfm_client,
    })
    .await
    {
        tracing::event!(
            name: "server.run.failed",
            tracing::Level::ERROR,
            error = %e,
            "server failed"
        );
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "suboxide=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Setup database
    let pool = match setup_database(&cli.database) {
        Ok(pool) => pool,
        Err(e) => {
            tracing::event!(
                name: "db.setup.failed",
                tracing::Level::ERROR,
                error = %e,
                "database setup failed"
            );
            std::process::exit(1);
        }
    };

    match cli.command {
        Some(Commands::User(cmd)) => {
            if let Err(e) = run_user_command(&pool, cmd) {
                tracing::error!(error = %e, "User command failed");
                std::process::exit(1);
            }
        }
        Some(Commands::ApiKey(cmd)) => {
            if let Err(e) = run_api_key_command(&pool, cmd) {
                tracing::error!(error = %e, "API key command failed");
                std::process::exit(1);
            }
        }
        Some(Commands::Folder(cmd)) => {
            if let Err(e) = run_folder_command(&pool, cmd) {
                tracing::error!(error = %e, "Folder command failed");
                std::process::exit(1);
            }
        }
        Some(Commands::Lastfm(cmd)) => {
            if let Err(e) = run_lastfm_command(&pool, cmd).await {
                tracing::error!(error = %e, "Last.fm command failed");
                std::process::exit(1);
            }
        }
        Some(Commands::Scan {
            folder,
            directory,
            full,
        }) => {
            if let Err(e) = run_scan_command(&pool, folder, directory, full) {
                tracing::event!(
                    name: "scan.command.failed",
                    tracing::Level::ERROR,
                    error = %e,
                    "scan command failed"
                );
                std::process::exit(1);
            }
        }
        Some(Commands::Serve {
            auto_scan,
            auto_scan_interval,
        }) => {
            run_server_command_or_exit(pool, cli.port, auto_scan, auto_scan_interval).await;
        }
        None => {
            run_server_command_or_exit(pool, cli.port, false, 300).await;
        }
    }
}

/// Server configuration.
struct ServerConfig {
    pool: DbPool,
    port: u16,
    auto_scan: bool,
    auto_scan_interval: u64,
    lastfm_client: LastFmClient,
}

/// Errors that can occur during server startup.
#[derive(Debug, thiserror::Error)]
enum ServerError {
    #[error("Failed to check users: {0}")]
    UserCheck(#[source] UserRepoError),
    #[error("Failed to bind to address: {0}")]
    Bind(#[source] std::io::Error),
    #[error("Failed to get local address: {0}")]
    LocalAddr(#[source] std::io::Error),
    #[error("Failed to configure CORS: {0}")]
    CorsConfig(#[source] CorsConfigError),
    #[error("Server error: {0}")]
    Serve(#[source] std::io::Error),
}

async fn run_server(config: ServerConfig) -> Result<(), ServerError> {
    let pool = config.pool;
    let port = config.port;
    let auto_scan = config.auto_scan;
    let auto_scan_interval = config.auto_scan_interval;
    let lastfm_client = config.lastfm_client;

    // Check if there are any users
    let users = UserRepository::new(pool.clone())
        .find_all()
        .map_err(ServerError::UserCheck)?;
    if users.is_empty() {
        tracing::event!(
            name: "server.bootstrap.no_users",
            tracing::Level::WARN,
            "no users found in database"
        );
        tracing::event!(
            name: "server.bootstrap.user_create_hint",
            tracing::Level::WARN,
            command = "suboxide user create --username admin --password <password> --admin",
            "create initial user with command"
        );
    }

    let state = AppState::new(pool.clone(), lastfm_client);
    let cors_config = CorsConfig::from_env().map_err(ServerError::CorsConfig)?;
    let app = create_router(state.clone(), &cors_config);

    // Start auto-scanner if enabled, sharing the same scan state with the API
    let _auto_scan_handle = if auto_scan {
        let scan_state = state.scan_state();
        let auto_scanner = AutoScanner::with_interval(pool, scan_state, auto_scan_interval);
        tracing::event!(
            name: "scan.auto.enabled",
            tracing::Level::INFO,
            scan.interval_secs = auto_scan_interval,
            "auto-scan enabled"
        );
        Some(auto_scanner.start())
    } else {
        None
    };

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(ServerError::Bind)?;

    let local_addr = listener.local_addr().map_err(ServerError::LocalAddr)?;
    tracing::event!(
        name: "server.listen.started",
        tracing::Level::INFO,
        server.address = %local_addr,
        "suboxide server listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Serve)?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to install ctrl-c handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to install terminate handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::event!(
        name: "server.shutdown.started",
        tracing::Level::INFO,
        "shutdown signal received"
    );
}
