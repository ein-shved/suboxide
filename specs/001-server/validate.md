# Validation: server

## Automated Checks

### Check: Built-in project tests

- Covers: [REQ-001](requirements.md#req-001-music-server).
- Method: Run `nix develop --command cargo test` from the project root.
  Existing `router_contract` assertions cover GET/base and view ping,
  POST-form authentication, and JSON/XML authentication errors.
- Evidence: All built-in project tests pass.

### Check: ASD-01 Direct update isolation

- Covers: [REQ-002](requirements.md#req-002-partial-library-updates).
- Method: `tests/scan_domains.rs::asd_01`; run
  `nix develop --command cargo test --test scan_domains asd_01`.
  Index direct tracks in `A`, `A/child`, and sibling `B`; add, retag,
  and remove tracks in `A`. Update only `A` in incremental and full modes.
  Record directory
  enumeration, owned-song reads, and audio opens with test-local traces;
  compare persisted song rows.
- Evidence: Only `A` direct tracks change. Enumeration excludes child and
  sibling directories; audio opens include changed/new files in incremental
  mode and every direct audio file in full mode. Unchanged incremental
  files stay unopened. Existing-song reads return only `A`'s direct songs.
  Unchanged paths retain folder/song IDs; child/sibling metadata stays.

### Check: ASD-02 Hierarchy and failed discovery

- Covers: [Nodes](system.md#subsystem-directory-ownership-and-discovery).
- Method: `tests/scan_domains.rs::asd_02`; run
  `nix develop --command cargo test --test scan_domains asd_02`.
  Scan a root with trackless parents, direct and child tracks, and an
  image-only descendant. Add audio there and in a newly created directory;
  exercise recursive and explicit discovery. Remove direct tracks, then
  child tracks. Move a track
  between folders and update each scope. Remove a selected directory;
  repeat recursively. Inject listing, entry-classification, stat, and
  metadata-read failures; make the root missing or unreadable. Make an
  indexed descendant unreadable during recursive scans. Add a directory
  symlink. Inspect folder rows and each song's direct owner;
  vary root and descendant `enabled` values. Run `Root` and
  `AllEnabled` in both modes; record directory IDs and returned song owners
  at existing-song reads and reconciliation with test-local observation.
- Evidence: The `music_folders` hierarchy has unique absolute paths; roots
  have null `parent`, generated folders reference their immediate
  containing folders, and ancestry reaches the registered root.
  Each song's single `music_folder_id` references its direct container;
  only root `enabled` controls scanning. New audio creates its folder
  and ancestors. Image-only leaves create no nodes. Trackless parents
  survive while children exist; empty leaves prune to the root.
  Moves reconcile only the updated scopes. Proven absence removes direct
  songs on selection, retaining indexed children and needed ancestors;
  recursive absence removes the branch. Incomplete listings/classification
  preserve unproven absences; stat/metadata errors preserve found songs.
  Root failures preserve the index; an unreadable descendant preserves
  its branch's songs and nodes. Failures are reported. Directory
  symlinks are not traversed. Root/all-root execution loads and reconciles
  songs per directory.
  Observed reads return only their owner's songs.

### Check: ASD-03 Root selection and public filters

- Covers: [Roots](system.md#subsystem-root-identity-and-update-interfaces).
- Method: `tests/scan_domains.rs::asd_03`; run
  `nix develop --command cargo test --test scan_domains asd_03`.
  Register two roots with nested folders and a shared album. Test equal,
  ancestor, and descendant canonical registrations, including disabled
  roots. Exercise CLI and admin `startScan` with all, root, and directory
  targets in both modes; include redundant separators, `.`, empty and
  missing paths, absolute paths, `..`,
  symlink/non-directory components, unknown/disabled/generated root IDs,
  directory-without-root, and filesystem access errors. Try generated IDs
  in root removal and enable/disable operations. Compare resolved targets
  and touched folder IDs for valid selectors. Repeat as non-admin and
  while busy. Query public folders, paginated song/artist/album views,
  and folder browsing with each root, unknown/generated IDs, and no filter.
  Remove one root.
- Evidence: CLI and API choose matching all/root/directory scopes.
  Normalized paths select the intended direct directory; empty paths
  and `.` select root direct entries only.
  Missing paths remain valid; only proof-backed absence removes selected
  direct songs, retaining indexed child branches. Failures preserve data
  and yield unsuccessful CLI exits and diagnostics. Equal, ancestor, and
  descendant canonical registrations are rejected, including disabled
  roots. API errors are 10 for directory-without-root,
  70 for unknown/generated root IDs, 0 for other
  selection failures, and 50 for non-admin requests. Busy requests retain
  status without launching another scan; accepted/status responses retain
  progress fields. Root selection rejects disabled roots; all-root scans
  exclude them and fail with `NoMusicFolders` when none are enabled.
  Scanner errors distinguish `FolderNotFound`, `InvalidTarget`,
  `NoMusicFolders`, and I/O.
  Generated IDs cannot be administered as roots. Public folders expose
  enabled null-parent IDs/names; administration lists all null-parent roots.
  Filters include nested members before pagination; unknown/generated IDs
  match nothing, and unfiltered views retain disabled-root tracks.
  Shared album details remain global. Root removal preserves the other
  root's songs and reconciles their shared album.

### Check: ASD-04 Shared album reconciliation

- Covers: [REQ-003](requirements.md#req-003-shared-album-integrity).
  [Albums](system.md#subsystem-existing-album-behavior).
- Method: `tests/scan_domains.rs::asd_04`; run
  `nix develop --command cargo test --test scan_domains asd_04`.
  Seed an album across two roots and directories with unequal durations.
  Retag one member into another album, remove selected members, disable an
  untouched root, then remove every remaining member. Record sibling audio
  opens. Include distinct album artists, track-artist fallback, missing
  artist, and missing album. Reread changed year/genre and embedded art with
  missing and existing album covers; include a track without embedded art
  and a usable image beside it.
- Evidence: Old and new albums reflect all remaining members, including
  disabled roots. Track lists, counts, and duration sums agree; sibling
  audio stays unopened. Last-member removal deletes the album. Existing
  identity and insert-time year/genre remain. Missing covers use embedded
  art then adjacent-image fallback; existing covers are reused. Albumless
  songs have no membership.

## Manual Checks

### Check: Recursive scan orchestration

- Covers: [Nodes](system.md#subsystem-directory-ownership-and-discovery).
  [Targets](system.md#subsystem-root-identity-and-update-interfaces).
- Method: Inspect recursive orchestration, every scanner entry point, and
  `AutoScanner` dispatch in `src/scanner/engine.rs`; follow callers in
  `src/main.rs` and `src/api/handlers/scanning.rs`.
- Evidence: Recursive root/all-root processing uses per-directory existing
  song sets without a root-wide existing-song map. The timer invokes
  incremental `AllEnabled` through `Scanner::scan`; CLI/API use that route
  with the selected target and mode. Every retained entry point reaches
  the same per-directory reconciliation boundary.
