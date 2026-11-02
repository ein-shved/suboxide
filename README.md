# Suboxide

A lightweight, self-hosted music streaming server implementing some of the [Subsonic API](http://www.subsonic.org/pages/api.jsp) and [OpenSubsonic](https://opensubsonic.netlify.app/) extensions, written in Rust.

> **Note** This is a personal use project built to fit specific needs. As a result, not all endpoints from the full Subsonic API specification are implemented. It includes the most commonly used features for streaming music and managing libraries. For production use cases requiring comprehensive API coverage, consider using [Navidrome](https://www.navidrome.org/) or other established Subsonic implementations.

## Features

- **Subsonic API Compatible** - Works with any Subsonic-compatible client (DSub, Symfonium, Submariner, etc.)
- **OpenSubsonic Extensions** - Supports `formPost`, `apiKeyAuthentication`, `songLyrics`, and `remoteControl`
- **Last.fm Integration** - Automatic scrobbling, now playing updates, and artist info/image fetching
- **Fast & Lightweight** - Built with Rust, Axum, and SQLite for minimal resource usage
- **Easy Setup** - Single binary with SQLite database, no external dependencies
- **Music Library Scanning** - Automatically scans and indexes your music collection
- **Remote Control Sessions** - Pair devices with a short code, send commands, and sync playback state
- **User Management** - Multi-user support with role-based permissions

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/imaviso/suboxide.git
cd suboxide

# Build with Cargo
cargo build --release

# The binary will be at target/release/suboxide
```

### With Nix

```bash
nix develop  # Enter development shell
cargo build --release
```

### As a NixOS Service

If you use flakes, import this repo's module and enable `services.suboxide`:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    suboxide.url = "github:imaviso/suboxide";
  };

  outputs = { nixpkgs, suboxide, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        suboxide.nixosModules.default
        ({ ... }: {
          services.suboxide = {
            enable = true;
            openFirewall = true;

            # Optional tuning
            port = 4040;
            autoScan = true;
            autoScanInterval = 300;

            # Runtime data
            dataDir = "/var/lib/suboxide";
            databasePath = "/var/lib/suboxide/suboxide.db";

            # If true (default), adds `suboxide` CLI to system PATH
            # and auto-uses `databasePath` (plus SUBOXIDE_DATA_DIR=dataDir)
            # unless --database is passed
            addToSystemPackages = true;

            # Secrets and runtime environment
            environmentFile = "/run/secrets/suboxide.env";
            environment = {
              RUST_LOG = "suboxide=info";
            };
          };
        })
      ];
    };
  };
}
```

Example `/run/secrets/suboxide.env`:

```bash
LASTFM_API_KEY=your_api_key
LASTFM_API_SECRET=your_api_secret
```

Note: cover art is stored under `services.suboxide.dataDir + "/covers"`.

## Quick Start

```bash
# 1. Create an admin user
./suboxide user create --username admin --password yourpassword --admin

# 2. Add your music folder
./suboxide folder add --name "Music" --path /path/to/your/music

# 3. Scan your library
./suboxide scan

# 4. Start the server
./suboxide serve
```

The server will start on `http://localhost:4040` by default.

## Configuration

### Command Line Options

```
Usage: suboxide [OPTIONS] [COMMAND]

Commands:
  user      Manage users
  api-key   Manage API keys
  folder    Manage music folders
  lastfm    Manage Last.fm integration
  scan      Scan music folders for audio files
  serve     Start the server (default)

Options:
  -d, --database <FILE>  Database file path [default: suboxide.db]
  -p, --port <PORT>      Server port [default: 4040]
  -h, --help             Print help
```

### Last.fm Integration

To enable Last.fm features (scrobbling and metadata fetching), set your API credentials:

```bash
export LASTFM_API_KEY="your_api_key"
export LASTFM_API_SECRET="your_api_secret"
```

Then, link a user account to Last.fm:

```bash
# Follow the interactive prompt to authorize
./suboxide lastfm link --username your_username
```

### Environment Variables

- `RUST_LOG` - Set log level (e.g., `suboxide=debug,tower_http=debug`)
- `LASTFM_API_KEY` - Last.fm API Key
- `LASTFM_API_SECRET` - Last.fm API Secret

## API Endpoints

### Implemented (72 endpoints)

| Category | Endpoints |
|----------|-----------|
| **System** | `ping`, `getLicense`, `getOpenSubsonicExtensions`, `tokenInfo` |
| **Browsing** | `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getArtists`, `getArtist`, `getAlbum`, `getSong`, `getAlbumList`, `getAlbumList2`, `getGenres`, `getArtistInfo`, `getArtistInfo2`, `getAlbumInfo`, `getAlbumInfo2`, `getSimilarSongs`, `getSimilarSongs2`, `getTopSongs`, `getRandomSongs`, `getSongsByGenre`, `search`, `search2`, `search3`, `getLyrics`, `getLyricsBySongId` |
| **Playlists** | `getPlaylists`, `getPlaylist`, `createPlaylist`, `updatePlaylist`, `deletePlaylist` |
| **Media Retrieval** | `stream`, `download`, `getCoverArt`, `getAvatar` |
| **Annotation** | `star`, `unstar`, `getStarred`, `getStarred2`, `scrobble`, `setRating`, `reportPlayback`, `getNowPlaying` |
| **Bookmarks** | `getBookmarks`, `createBookmark`, `deleteBookmark` |
| **Play Queue** | `getPlayQueue`, `savePlayQueue`, `getPlayQueueByIndex`, `savePlayQueueByIndex` |
| **Internet Radio** | `getInternetRadioStations`, `createInternetRadioStation`, `updateInternetRadioStation`, `deleteInternetRadioStation` |
| **Remote Control** | `createRemoteSession`, `joinRemoteSession`, `getRemoteSession`, `closeRemoteSession`, `sendRemoteCommand`, `getRemoteCommands`, `updateRemoteState`, `getRemoteState` |
| **User Management** | `getUser`, `getUsers`, `createUser`, `updateUser`, `deleteUser`, `changePassword` |
| **Scanning** | `startScan`, `getScanStatus` |

### OpenSubsonic Extensions

The server advertises the following extensions from `getOpenSubsonicExtensions`:

- `apiKeyAuthentication`
- `formPost`
- `indexBasedQueue`
- `playbackReport`
- `remoteControl`
- `songLyrics`

### API Notes

- `getAlbumList` and `getAlbumList2` can page through the full album catalog. Use `type=all`, `type=alphabeticalByName`, or omit `type` to browse all albums alphabetically.
- `download` accepts song, album, artist, and playlist ids. Albums and artists download as a zip archive (with `Disc NN/` subfolders for multi-disc albums); playlists download as numbered tracks plus an M3U playlist file.
- API ids are namespaced per entity type (navidrome-style): `mf-` for songs, `al-` for albums, `ar-` for artists, `pl-` for playlists. Bare integers are accepted as song ids for backwards compatibility.
- `getIndexes` honors the `ifModifiedSince` parameter, returning an empty index list with the current `lastModified` when nothing has changed.
- `startScan` is admin-only. `fullScan=true` rereads every audio file in the selected scope; otherwise scanning is incremental.
- Remote control support is exposed through the OpenSubsonic `remoteControl` extension. It supports host/controller pairing, queued commands, and playback state synchronization.

### Authentication

The server supports three authentication methods:

1. **Token Authentication** (recommended) - MD5(password + salt) via `t` and `s` parameters
2. **API Key** (OpenSubsonic) - Via `apiKey` parameter
3. **Legacy Password** - Plain or hex-encoded via `p` parameter

## Supported Audio Formats

The scanner recognizes the following formats:
- FLAC, MP3, AAC/M4A, OGG/Opus, WAV, AIFF, WMA, APE, WavPack

## Client Compatibility

Tested with:
- [Symfonium](https://symfonium.app/) (Android)
- [DSub](https://github.com/daneren2005/Subsonic) (Android)
- [Submariner](https://submarinerapp.com/) (macOS)
- [Sonixd](https://github.com/jeffvli/sonixd) (Desktop)
- [Feishin](https://github.com/jeffvli/feishin) (Desktop)

## Development

```bash
# Enter nix development shell (includes Rust toolchain)
nix develop

# Run in development mode
cargo run -- serve

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run linter
cargo clippy
```

## Scan selection

`suboxide scan` scans enabled registered roots recursively. Use `--folder ID`
for one root, or `--folder ID --directory PATH` for only that directory's
direct audio files. `PATH` is root-relative; `.` or an empty path selects the
root's direct files. Add `--full` to reread unchanged files in that scope.
Directory updates leave child and sibling tracks untouched.

Admin `startScan` uses the same selection through `musicFolderId`, `directory`,
and `fullScan`. Directory selection requires a root ID. Absolute paths, `..`,
symlink components, generated-folder IDs, and disabled roots are rejected.
Missing paths remove indexed tracks only when a successful listing proves
their absence; filesystem failures preserve unproven entries.

`folder list` shows registered roots, including disabled roots;
`getMusicFolders` shows enabled roots. Public `musicFolderId` filters include
nested songs, artists, and albums before pagination. Unknown and generated
IDs match nothing. Unfiltered views retain disabled roots' indexed tracks;
shared album details include all indexed members. Removing a root deletes
its indexed subtree and updates affected albums, retaining other roots' songs.

## Project Structure

```
src/
├── main.rs          # CLI and server entry point
├── lib.rs           # Library exports
├── api/             # Subsonic API implementation
│   ├── auth.rs      # Authentication middleware
│   ├── handlers/    # API endpoint handlers
│   └── response.rs  # Response formatting (XML/JSON)
├── db/              # Database layer (Diesel + SQLite)
├── models/          # Domain models
├── scanner/         # Music library scanner
└── crypto/          # Password hashing
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Acknowledgments

- [Subsonic](http://www.subsonic.org/) - Original API specification
- [OpenSubsonic](https://opensubsonic.netlify.app/) - Modern API extensions
- [Navidrome](https://www.navidrome.org/) - Inspiration for implementation
