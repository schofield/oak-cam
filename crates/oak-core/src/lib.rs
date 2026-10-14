//! oak-cam's core, shared by the CLI and the GUI; no GUI or CLI
//! dependencies.
//!
//! - [`config`]: the TOML configuration model, validation, atomic save.
//! - [`controls`]: config → camera controls (full sets and minimal deltas).
//! - [`capture`]: per-sensor capture threads and their statistics.
//! - [`vcam_sink`]: the frame sink that publishes to an OS virtual camera.
//! - [`thumb`]: rate-limited preview thumbnails for a GUI.
//! - [`supervisor`]: the thread that owns the device and pipeline, applies
//!   config changes live or by restarting, and reconnects.

pub mod capture;
pub mod config;
pub mod controls;
pub mod supervisor;
pub mod thumb;
pub mod vcam_sink;
