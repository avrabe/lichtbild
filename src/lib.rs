//! lichtbild: a read-only index of a household's photo folders, with a
//! server-rendered timeline. See docs/BRIEF.md and artifacts/ for the
//! requirements and the hazards each module guards against.

pub mod auth;
pub mod best;
pub mod config;
pub mod datadir;
pub mod dups;
pub mod embed;
pub mod faces;
pub mod fixes;
pub mod index;
pub mod map;
pub mod media;
pub mod nav;
pub mod objects;
pub mod ocr;
pub mod people;
pub mod place;
pub mod places;
pub mod scan;
pub mod search;
pub mod source;
pub mod status;
pub mod stt;
pub mod timing;
pub mod trips;
pub mod video;
pub mod vision;
pub mod web;
pub mod words;

/// Version string the binary reports (`lichtbild --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
