//! Music library scanner module.

pub mod cue;
pub mod engine;
pub mod state;
pub mod types;

// Re-export core types for backward compatibility
#[doc(inline)]
pub use engine::{AutoScanHandle, AutoScanner, Scanner};
#[doc(inline)]
pub use state::{ScanPhase, ScanState, ScanStateHandle};
#[doc(inline)]
pub use types::{ScanError, ScanMode, ScanResult, ScanTarget, ScannedTrack};
