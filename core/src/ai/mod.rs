//! AI noise reduction: RawNIND UtNet2 (Brummer & De Vleeschouwer 2025) run
//! with our own compute shaders. See `models/README.md` for the weights.

mod weights;

pub use weights::{Tensor, Variant, Weights, models_dir};
