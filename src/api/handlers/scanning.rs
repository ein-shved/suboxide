//! Library scanning API handlers (startScan, getScanStatus)

use axum::response::IntoResponse;
use serde::Deserialize;

use crate::api::auth::SubsonicContext;
use crate::api::handlers::util;
use crate::api::response::{ScanStatusData, SubsonicResponse};
use crate::scanner::{ScanError, ScanMode, ScanTarget, Scanner};

/// Build a `ScanStatusData` from the current scan state.
fn build_scan_status_data(auth: &SubsonicContext) -> ScanStatusData {
    let scan_state = auth.scan_state().snapshot();
    ScanStatusData {
        scanning: scan_state.scanning,
        count: scan_state.count,
        total: scan_state.total,
        phase: scan_state.phase,
        folder: scan_state.current_folder,
    }
}

/// Query parameters for startScan.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct StartScanParams {
    /// Whether to re-scan all files regardless of modification time.
    pub full_scan: Option<bool>,
    pub music_folder_id: Option<i32>,
    pub directory: Option<String>,
}

/// GET/POST /rest/startScan[.view]
///
/// Initiates a media library scan. If a scan is already in progress,
/// returns the current status without starting a new scan.
/// Admin only. Without `fullScan`, performs an incremental scan.
///
/// Returns: scanStatus with scanning=true/false and count of items scanned.
pub async fn start_scan(
    crate::api::auth::SubsonicQuery(params): crate::api::auth::SubsonicQuery<StartScanParams>,
    auth: SubsonicContext,
) -> impl IntoResponse {
    if !auth.user.is_admin() {
        return util::unauthorized(&auth);
    }

    let scan_state = auth.scan_state().clone();
    let pool = auth.pool().clone();
    let Some(guard) = scan_state.try_start() else {
        let data = build_scan_status_data(&auth);
        return SubsonicResponse::scan_status(auth.format, data).into_response();
    };

    let target = match (params.music_folder_id, params.directory) {
        (Some(root_id), Some(path)) => ScanTarget::Directory {
            root_id,
            path: path.into(),
        },
        (Some(root_id), None) => ScanTarget::Root(root_id),
        (None, None) => ScanTarget::AllEnabled,
        (None, Some(_)) => return util::missing_param(&auth, "musicFolderId"),
    };
    let scanner = Scanner::new(pool);
    let target = match scanner.resolve_target(target) {
        Ok(target) => target,
        Err(ScanError::FolderNotFound(_)) => return util::not_found(&auth, "Music folder"),
        Err(error) => return util::service_error(&auth, error.to_string()),
    };

    let mode = if params.full_scan.unwrap_or(false) {
        ScanMode::Full
    } else {
        ScanMode::Incremental
    };

    // Spawn background task to run the scan.
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            scanner.scan(target, mode, Some(&scan_state))
        })
        .await;

        match result {
            Ok(Ok(stats)) => {
                tracing::info!(
                    name = "scan.manual.completed",
                    tracks.found = stats.tracks_found,
                    tracks.added = stats.tracks_added,
                    tracks.failed = stats.tracks_failed,
                    "manual scan completed"
                );
            }
            Ok(Err(e)) => {
                tracing::error!(name = "scan.manual.failed", error = %e, "manual scan failed");
            }
            Err(e) => {
                tracing::error!(
                    name = "scan.manual.task_panic",
                    error = %e,
                    "manual scan task panicked"
                );
            }
        }
    });

    // Return current status (scanning should be true now)
    let data = build_scan_status_data(&auth);
    SubsonicResponse::scan_status(auth.format, data).into_response()
}

/// GET/POST /rest/getScanStatus[.view]
///
/// Returns the current status of the media library scan.
///
/// Returns: scanStatus with scanning=true/false and count of items scanned.
pub async fn get_scan_status(auth: SubsonicContext) -> impl IntoResponse {
    let data = build_scan_status_data(&auth);
    SubsonicResponse::scan_status(auth.format, data)
}
