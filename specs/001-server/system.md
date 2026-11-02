# System Specification

SpecD was adopted late in the development of this existing project.
The factual [project inventory](inventory.md) gives agents
quick orientation to the implementation serving
[REQ-001](requirements.md#req-001-music-server).

## Scan architecture overview

- Operators register music roots through the CLI and request root or
  directory updates through CLI/API entry points, passing the scope to
  the scanner. The
  [root interfaces](#subsystem-root-identity-and-update-interfaces)
  section owns registration, selection, and public folder contracts for
  [REQ-002](requirements.md#req-002-partial-library-updates).
- Music repositories persist one `music_folders` hierarchy. A null `parent`
  identifies a user-registered root; a non-null `parent` identifies an
  automatically discovered directory. `songs.music_folder_id` references
  the directly containing folder. This ownership serves
  [REQ-002](requirements.md#req-002-partial-library-updates); its storage
  and lifecycle contracts belong to
  [directory discovery](#subsystem-directory-ownership-and-discovery).
- The scanner starts with the selected root and maintains its hierarchy.
  Each directory update discovers and reconciles direct tracks through
  music repositories; child directories own their own tracks. Recursive
  root scans apply that boundary throughout the tree and discover audio
  beneath trackless or image-only directories. Existing periodic scans
  visit enabled roots. These flows serve
  [REQ-002](requirements.md#req-002-partial-library-updates); see
  [directory discovery](#subsystem-directory-ownership-and-discovery).
- Browsing APIs and music repositories resolve public root filters by
  traversing the root's folder subtree to its songs. Media handlers use
  the shared [audio resolver](#subsystem-flaccue-tracks). These serve
  [REQ-001](requirements.md#req-001-music-server);
  [root interfaces](#subsystem-root-identity-and-update-interfaces)
  owns public root visibility and filtering.
- Scanner ingest and scoped removal pass affected old and new album IDs
  to reconciliation. Music repositories refresh counts and durations
  from persisted members; browsing queries global album membership across
  folders and roots. Identity, metadata, artwork, and reconciliation serve
  [REQ-003](requirements.md#req-003-shared-album-integrity); see
  [album behavior](#subsystem-existing-album-behavior).

## Subsystem: Directory ownership and discovery

- Music repositories store roots and discovered directories in
  `music_folders`: `id`, `name`, `path`, `enabled`, timestamps, and nullable
  `parent` as a foreign key to `music_folders.id`.
  `parent = NULL` identifies a registered root; otherwise it identifies
  the immediate containing directory. Every `path` is absolute and unique.
  Descendants reach their root through `parent`; only the root's `enabled`
  controls scanning of that tree. This realizes
  [REQ-002](requirements.md#req-002-partial-library-updates).
- For [REQ-002](requirements.md#req-002-partial-library-updates), each
  song's `music_folder_id` references its directly containing folder.
  Source paths remain absolute. An unchanged ordinary audio path retains
  its folder and song IDs; [CUE track identity](#subsystem-flaccue-tracks)
  owns image-backed songs. Registration, target selection, root lifecycle,
  and public folder contracts belong to
  [root interfaces](#subsystem-root-identity-and-update-interfaces).
- For [REQ-002](requirements.md#req-002-partial-library-updates), discovery
  creates a folder when supported audio is found directly inside it and
  creates any missing ancestor nodes up to the registered root. Generated
  names use directory names. A node remains while it owns indexed songs
  or connects descendants. Successfully confirmed empty leaves
  are pruned upwards when they have no supported audio, songs, or children;
  registered roots remain even when empty. Image-only directories create
  nodes only when needed as ancestors of audio-containing directories.
- Scanner direct-directory work for
  [REQ-002](requirements.md#req-002-partial-library-updates) loads existing
  songs only by that folder's ID and lists only its direct entries.
  It ingests supported audio there, skips unchanged ordinary audio using
  modification time and size checks in incremental mode, and reconciles
  absent direct songs. [CUE pairs](#subsystem-flaccue-tracks) use their
  source fingerprint and track set in the same unit. Full mode rereads
  the same direct scope. Child and sibling songs remain unchanged by this unit.
- Recursive root and all-enabled-root scans satisfy
  [REQ-002](requirements.md#req-002-partial-library-updates) by traversing
  directories and applying that direct unit independently to each one.
  Directory symlinks are not traversed. These scans do not load the root's
  entire song set. Traversal includes trackless
  and image-only directories, so newly added audio below them is discovered
  on a subsequent recursive pass. Existing periodic incremental scans use
  this same enabled-root route; automatic change tracking remains deferred.
- Absence reconciliation for
  [REQ-002](requirements.md#req-002-partial-library-updates) uses complete,
  successful direct directory listings. Discovered audio paths and CUE
  sources are counted as present before file stats or metadata parsing,
  so those failures preserve their existing songs. Listing, entry
  classification, stat, and metadata failures are reported. An incomplete
  listing preserves songs and nodes whose absence it cannot prove;
  successful entries may still be ingested.
- For [REQ-002](requirements.md#req-002-partial-library-updates), a complete
  parent listing can prove a directory absent. A recursive pass removes
  indexed songs and generated nodes in that absent branch within its scope.
  A selected direct update may list its immediate parent to prove the
  selected directory absent, but removes only its own direct songs and
  retains ancestor nodes needed by indexed children. A missing or unreadable
  root preserves its indexed tree; an unreadable descendant preserves its
  indexed branch. Neither condition is treated as an empty listing.
- For [REQ-002](requirements.md#req-002-partial-library-updates), filesystem
  moves reconcile as a newly discovered destination path and a source path
  removed only when its owning directory is covered and absence is proven.
  Destination ownership and any new ancestors follow normal discovery;
  a selected destination update leaves an unvisited source unchanged.
- Ingest and scoped removal collect old and new affected album IDs for
  [REQ-003](requirements.md#req-003-shared-album-integrity) and pass them to
  [album reconciliation](#subsystem-existing-album-behavior). Reconciliation
  reads persisted membership across folders without scanning sibling audio.

## Subsystem: Root identity and update interfaces

- For [REQ-002](requirements.md#req-002-partial-library-updates),
  `MusicFolderRepository` registers an existing directory at its canonical
  absolute path. Registration rejects equal, ancestor, or descendant paths
  of any registered root, including disabled roots. Administrative add,
  remove, and enable/disable operations manage only registered roots;
  their IDs are `music_folders` IDs with `parent = NULL`. Tree ownership
  and the root's `enabled` control belong to
  [directory discovery](#subsystem-directory-ownership-and-discovery).
- For [REQ-002](requirements.md#req-002-partial-library-updates),
  `folder remove --id ID` deletes that registered root, its folder subtree,
  and all songs owned in that subtree through music repositories.
  It passes removed songs' old album IDs to
  [album reconciliation](#subsystem-existing-album-behavior) for
  [REQ-003](requirements.md#req-003-shared-album-integrity), retaining
  shared albums' persisted members in other roots.
- For [REQ-002](requirements.md#req-002-partial-library-updates), the
  scanner defines `ScanTarget`: `AllEnabled`, `Root(root_id)`, or
  `Directory { root_id, path }`. `Scanner::scan(target, mode, state)` takes
  this target, `ScanMode`, and optional progress state. Root targets
  recurse; directory targets follow the
  [direct scope](#subsystem-directory-ownership-and-discovery), including
  directories without a stored folder. `AllEnabled` selects enabled
  registered roots and fails with `NoMusicFolders` when none exist.
- For [REQ-002](requirements.md#req-002-partial-library-updates), explicit
  unknown root IDs or IDs of generated folders fail with `FolderNotFound`;
  disabled roots fail with `InvalidTarget`. Directory paths are relative to
  the selected root: normalize redundant separators and `.`; an empty path
  or `.` selects root direct entries. Reject absolute paths, `..`
  components, and existing components that are symlinks or non-directories.
  Missing components remain valid for
  [absence reconciliation](#subsystem-directory-ownership-and-discovery);
  other filesystem access errors fail selection. Invalid targets leave
  indexed data intact. Discovery owns filesystem failure and removal rules.
- For [REQ-002](requirements.md#req-002-partial-library-updates),
  `scan --folder ID --directory PATH` selects a directory; `--directory`
  requires `--folder`. Folder alone selects a recursive root scan; omitting
  both selects `AllEnabled`. `--full` selects full mode; otherwise mode is
  incremental. `run_scan_command` resolves targets through the scanner and
  retains CLI progress and completion statistics. Selection failures exit
  unsuccessfully with a diagnostic.
- For [REQ-002](requirements.md#req-002-partial-library-updates), admin-only
  `startScan` accepts optional `musicFolderId` and `directory` with the same
  selection rules; `fullScan` controls mode. The server's shared scan guard
  returns current status when busy. Otherwise, the handler resolves the
  target before background launch. Accepted requests and `getScanStatus`
  retain their scan-status response and existing progress fields.
- For [REQ-002](requirements.md#req-002-partial-library-updates), the API
  returns Subsonic error 10 for `directory` without `musicFolderId`, 70 for
  `FolderNotFound`, and 0 for other selection failures. Scanner errors
  distinguish `FolderNotFound`, `InvalidTarget`, `NoMusicFolders`, and I/O.
  Runtime failures retain existing scan failure reporting.
- For [REQ-001](requirements.md#req-001-music-server), `getMusicFolders`
  lists enabled registered roots with integer root IDs and names.
  `folder list` lists registered roots including disabled roots. Disabled
  roots' indexed tracks remain available in library views until removed.
- For [REQ-001](requirements.md#req-001-music-server), `MusicLibrary` and
  music repositories interpret public `musicFolderId` as a registered-root
  ID. Filters traverse that root's folder subtree to directly owned songs.
  Song results use matching tracks; artist and album results require at
  least one matching track, with filtering before pagination. Unknown IDs
  and generated-folder IDs match nothing; omitted filters query all indexed
  tracks. Folder browsing uses the same membership rule. Shared album
  identity and [album data](#subsystem-existing-album-behavior) remain
  global.

## Subsystem: Existing album behavior

[REQ-003](requirements.md#req-003-shared-album-integrity)

- Music repositories identify an album globally by exact album name and
  artist ID, resolving the album-artist tag with track artist as fallback.
  Missing artist uses a null key component; missing album leaves the song
  without album membership. Directory and root IDs are outside this key.
- Scanner ingest retains the existing album lookup, metadata, artwork, and
  batch-write behavior. Album year and genre are set on album insertion.
  A missing album cover is filled from embedded art, falling back to the
  existing lookup beside the track; an existing cover is reused.
- Scanner ingest, scoped deletion, and root removal collect affected old
  and new album IDs. Reconciliation refreshes their song counts and summed
  durations from all persisted member songs, including unchanged songs in
  other directories and disabled roots, without rereading sibling audio.
  Repositories delete an album when its last member disappears. Album
  track lists query current membership; album data remain global even
  with a root filter.

## Subsystem: FLAC+CUE tracks

- For [REQ-004](requirements.md#req-004-flaccue-listening), scanner parsing
  accepts one external CUE referencing one same-directory FLAC, with numbered
  `TRACK AUDIO` entries and increasing `INDEX 01` times (1/75 second ticks).
  Each cut is the first source frame start at or after that rational time;
  adjacent tracks share the cut, and the last ends at EOF. Out-of-source or
  empty rounded intervals fail indexing. Duration uses selected whole frames.
- For [REQ-004](requirements.md#req-004-flaccue-listening), each CUE track is
  a song with its own metadata. Track title precedes image title; performer
  uses track, sheet, then image. Sheet title and performer precede image
  album and album-artist tags. CUE supplies track
  number; source artwork follows existing album behavior.

For [REQ-004](requirements.md#req-004-flaccue-listening), music repositories
persist CUE sources and logical tracks with their frame and byte boundaries
in SQLite. Fields listed for `cue_sources` and `cue_tracks` are `NOT NULL`;
the added `songs.cue_track_id` is nullable. CUE identity, metadata and
boundaries use typed columns, not JSON or opaque serialized structures.
Per-frame descriptors exist temporarily during scanning or media delivery.

| Table | Columns and keys |
| --- | --- |
| `cue_sources` | `id INTEGER PRIMARY KEY`; `sheet_path TEXT UNIQUE`, the absolute sheet path; `cue_content TEXT`, the original sheet text; `image_size INTEGER`; `image_modified_seconds INTEGER` and `image_modified_nanos INTEGER`, the image mtime since Unix epoch. |
| `cue_tracks` | `id INTEGER PRIMARY KEY`; `cue_source_id INTEGER REFERENCES cue_sources(id) DEFERRABLE INITIALLY DEFERRED`, with `NO ACTION` on source deletion; `track_number INTEGER`; `start_frame INTEGER` and `end_frame INTEGER`, a half-open ordinal interval; `byte_start INTEGER` and `byte_end INTEGER`, a half-open byte interval in the original FLAC, measured from the beginning of the file. Byte start points to the first selected frame header; byte end follows the last selected frame CRC-16 and is image EOF for the last track. `UNIQUE (cue_source_id, track_number)`. |
| `songs` addition | `cue_track_id INTEGER UNIQUE REFERENCES cue_tracks(id) DEFERRABLE INITIALLY DEFERRED`, with `NO ACTION` on track deletion. A null reference identifies an ordinary file. Existing `path` stores the original audio path and `music_folder_id` its direct owner. |

For [REQ-004](requirements.md#req-004-flaccue-listening), database `CHECK`
constraints on `cue_tracks` require positive `track_number`,
`0 <= start_frame < end_frame` and `0 <= byte_start < byte_end`.
A partial unique index on `songs(path)` where `cue_track_id IS NULL`
identifies ordinary files. Unique source sheet paths and CUE source/number
pairs identify logical CUE tracks; unique non-null song references give
each CUE track one song. Retained sheets keep their source IDs; the same
sheet/number retains its CUE track ID, song ID and personal references.

For [REQ-004](requirements.md#req-004-flaccue-listening),
`cue_tracks.track_number` identifies the entry within its sheet.
Existing `songs` columns retain the song's metadata, including its track
number, physical path, direct owner and audio properties. Scanner publication
sets `songs.track_number` to the linked CUE track number and audio properties
from the parsed FLAC. Domain/API metadata, sorting, tags and filenames use
those song columns. `songs.file_size` describes the logical standalone track;
`cue_sources.image_size` describes the physical image for fingerprint checks.

For [REQ-004](requirements.md#req-004-flaccue-listening), source `CHECK`
constraints require positive image size, nonnegative mtime seconds and
nanoseconds in `0..1_000_000_000`. The scanner's native parser validates
source frames and CRCs and temporarily maps their ordinals, byte offsets
and sample extents. Before publication, repositories check that all songs
linked to one source share an absolute image path, direct owner and
valid non-null audio properties matching that FLAC: positive sampling rate,
channels in `1..=8` and bit depth in `4..=32`. The image is the sheet's
referenced same-directory file. Song and CUE track numbers agree; both stored
intervals describe the same ceiling-selected whole frames, adjacent tracks
share their cuts, and interval ends do not exceed the temporary source frame
count or `image_size`. Inside the publication transaction, repositories
check joined songs, CUE tracks and sources together with staged replacements
to reject competing source owners of one image path.
Those boundaries belong to the source fingerprint.
After publication, each CUE track has one associated song and each source
has referenced tracks; only source metadata and track boundaries persist.

- For [REQ-002](requirements.md#req-002-partial-library-updates), the scanner
  prepares complete pairs before one direct-directory transaction publishes
  source metadata, CUE tracks with their boundaries and all affected songs together,
  replacing the ordinary image song. Each source's size and mtime bind the
  boundaries to one image revision; incremental checks compare CUE content
  and that fingerprint. Source and track replacement retain their IDs;
  deferred foreign keys permit affected rows to be replaced under the same IDs,
  including image ownership exchanges, with all references valid at commit.
  Changes rebuild the pair, removed sheets restore ordinary image indexing,
  and proven source absence removes tracks. Parse/read/index failures preserve
  indexed state; publication failures roll back all three tables' changes.
  Permanent track or source removal deletes or reassigns affected songs and
  removes unreferenced CUE tracks and sources in the same transaction.
  Direct updates and root removal leave no dangling references or
  unreferenced CUE tracks and sources.
  Media rejects changed source fingerprints until rescan.
  Affected albums use [REQ-003](requirements.md#req-003-shared-album-integrity)
  [reconciliation](#subsystem-existing-album-behavior).
- For [REQ-004](requirements.md#req-004-flaccue-listening), stream, download,
  and ZIP share a resolver that reads the song, its `cue_tracks` row through
  `cue_track_id`, and source metadata through `cue_source_id` in one database
  transaction. It uses the song's physical path, owner and audio properties,
  authorizes that original source against configured roots, opens it and
  checks its size and precise mtime against `cue_sources`.
  The native parser seeks to the CUE track's `byte_start` and parses only
  its `[byte_start, byte_end)` interval using the song's audio parameters,
  walking actual subframe lengths and validating headers and frame CRCs.
  It builds a request-local frame map, checks the parsed frame count against
  the stored ordinal interval, sums selected samples and rechecks the opened
  source fingerprint before responding. The native reader
  emits virtual standalone FLAC with regenerated STREAMINFO and track tags,
  fixed frame numbers rebased from the first selected ordinal or variable
  sample numbers rebased from the first selected sample number, updated
  header/frame CRCs, and unchanged compressed subframe payload. STREAMINFO
  counts output samples with MD5 zero (unavailable); the original SEEKTABLE
  is omitted. The temporary map and transformed offsets define deterministic
  output length and byte ranges. Every request, including a narrow Range,
  parses that song interval before responding; source bytes preceding the
  interval are not scanned. HTTP Range addresses the generated standalone
  FLAC, not source-file offsets. Filenames include track number. There is no
  audio reencoding, extra audio file, or FFmpeg runtime dependency; ordinary
  media behavior stays.

Future design notes for [REQ-004](requirements.md#req-004-flaccue-listening):

1. If real user listening experience reveals boundary problems, investigate
   hybrid processing/reencoding of small boundary regions while retaining
   original bulk frames.
2. If hybrid processing is too costly or complex, reconsider pre-extracted
   standalone disk copies. These notes require future design decisions.
