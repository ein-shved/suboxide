# Requirements

## REQ-001: Music server

suboxide must serve as a music server.

### Scenario: Listen remotely

- WHEN a user wants to listen to music remotely
- THEN the user can connect to suboxide and listen to music, similarly to
  using Navidrome

### Scenario: Accept a limited feature set

- WHEN suboxide lacks a Navidrome capability that no acceptance scenario
  explicitly requires
- THEN that omission is acceptable; full Navidrome feature parity is
  unnecessary

## REQ-002: Partial library updates

An operator must be able to reflect modifications to part of the library
without restarting indexing of the entire library.

### Scenario: Update a changed part of the library

- WHEN the operator updates part of the library after tracks are added,
  modified, or removed
- THEN the library reflects those additions, changes, and removals
- AND indexed tracks elsewhere remain available without a full reindex

## REQ-003: Shared album integrity

Listeners must see a coherent logical album when its tracks span
independently updated parts of the library.

### Scenario: Remove tracks from part of a shared album

- GIVEN one album has indexed tracks in two parts of the library
- WHEN an update removes some or all tracks from one part
- THEN the album still includes its indexed tracks from the other part
- AND its track list, track count, and duration reflect the
  remaining indexed collection

### Scenario: Change metadata in part of a shared album

- GIVEN one album has indexed tracks in two parts of the library
- WHEN an update changes track metadata in one part, including a track's
  album membership
- THEN each affected album's track list, track count, and duration reflect
  the updated indexed collection
- AND indexed tracks in the other part remain available
