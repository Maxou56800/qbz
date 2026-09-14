//! Frontend-agnostic text/date utilities extracted from the qbz binary.
pub mod dates;
pub mod names;
pub mod sleep;
pub mod strip_html;

pub use sleep::format_sleep_remaining;
