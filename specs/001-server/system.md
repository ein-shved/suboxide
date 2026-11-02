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
  traversing the root's folder subtree to its songs. Media handlers open
  the stored absolute `song.path`. These serve
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
  `song.path` remains absolute. An unchanged path retains its folder and
  song IDs. Registration, target selection, root lifecycle,
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
  It ingests supported audio there, skips unchanged metadata in incremental
  mode using existing modification time and size checks, and reconciles
  absent direct songs. Full mode rereads the same direct scope. Child and
  sibling songs remain unchanged by this unit.
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
  successful direct directory listings. Discovered audio paths are counted
  as present before file stats or metadata parsing, so those failures
  preserve existing songs. Listing, entry classification, stat, and metadata
  failures are reported. An incomplete listing preserves songs and nodes
  whose absence it cannot prove; successful entries may still be ingested.
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
