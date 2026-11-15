//! RawValley engine: UI-agnostic photo loading, GPU grading, LUT baking and
//! scopes. Front-ends talk to it through [`gpu::spawn`] jobs/results.

#[cfg(feature = "ai")]
pub mod ai;
pub mod ca;
pub mod chart;
pub mod crop;
pub mod develop;
pub mod gpu;
pub mod grade;
pub mod lab;
pub mod lut;
pub mod matching;
pub mod noise;
pub mod photo;
pub mod preset;
pub mod profiles;
pub mod raw;
pub mod scopes;
pub mod sidecar;
pub mod view;
