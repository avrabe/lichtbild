//! lichtbild: a read-only index of a household's photo folders, with a
//! server-rendered timeline. See docs/BRIEF.md and artifacts/ for the
//! requirements and the hazards each module guards against.

pub mod auth;
pub mod config;
pub mod datadir;
pub mod dups;
pub mod index;
pub mod map;
pub mod media;
pub mod nav;
pub mod place;
pub mod places;
pub mod scan;
pub mod search;
pub mod source;
pub mod status;
pub mod stt;
pub mod video;
pub mod web;
pub mod words;

/// Version string the binary reports (`lichtbild --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
