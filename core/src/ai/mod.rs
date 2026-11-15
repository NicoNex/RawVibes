//! AI noise reduction: RawNIND UtNet2 (Brummer & De Vleeschouwer 2025) run
//! with our own compute shaders. See `models/README.md` for the weights.

mod tile;
mod unet;
mod weights;

pub(crate) use tile::AiRun;
pub use unet::UNet;
pub use weights::{Tensor, Variant, Weights, models_dir};
