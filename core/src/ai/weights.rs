//! The converted RawNIND weights (`tools/convert_rawnind.py`): one flat f32
//! file per variant and the list of its tensors in graph order.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which UtNet2: packed RGGB in, demosaiced camera RGB out at twice the size
/// (Bayer sensors), or linear Rec.2020 in and out (every other sensor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Bayer,
    Linear,
}

impl Variant {
    pub(crate) fn stem(self) -> &'static str {
        match self {
            Variant::Bayer => "bayer",
            Variant::Linear => "linear",
        }
    }
}

/// A convolution's weight or bias: `shape` as ONNX lays it out, its first
/// float at `offset` in [`Weights::data`].
#[derive(Clone, Debug, Deserialize)]
pub struct Tensor {
    pub name: String,
    pub shape: Vec<usize>,
    pub offset: usize,
}

impl Tensor {
    pub fn size(&self) -> usize {
        self.shape.iter().product()
    }
}

pub struct Weights {
    pub variant: Variant,
    /// Weight then bias of every Conv / ConvTranspose, in graph order.
    pub tensors: Vec<Tensor>,
    pub data: Vec<f32>,
}

impl Weights {
    pub fn load(dir: &Path, variant: Variant) -> Result<Self, String> {
        let stem = variant.stem();
        let read = |ext: &str| {
            let path = dir.join(format!("{stem}.{ext}"));
            std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let tensors: Vec<Tensor> = serde_json::from_slice(&read("json")?).map_err(|e| e.to_string())?;
        let bytes = read("bin")?;
        if bytes.len() % 4 != 0 {
            return Err(format!("{stem}.bin is not a list of f32"));
        }
        let data: Vec<f32> = bytemuck::pod_collect_to_vec(&bytes);
        if let Some(t) = tensors.iter().find(|t| t.offset + t.size() > data.len()) {
            return Err(format!("{stem}: tensor {} runs past the weights", t.name));
        }
        // UtNet2 reads these by name, each weight followed by its bias.
        let paired = tensors.chunks(2).all(|p| {
            p.len() == 2 && p[1].name == p[0].name.replace(".weight", ".bias") && p[0].name.ends_with(".weight")
        });
        let names: Vec<&str> = tensors.iter().map(|t| t.name.trim_end_matches(".weight")).collect();
        if !paired || !layer_names().all(|n| names.contains(&n.as_str())) {
            return Err(format!("{stem}: not the RawNIND UtNet2 tensor layout"));
        }
        Ok(Self { variant, tensors, data })
    }
}

/// Every layer `UNet` looks up by name.
fn layer_names() -> impl Iterator<Item = String> {
    let per_level = (1..=4).flat_map(|l| {
        [0, 2]
            .into_iter()
            .flat_map(move |i| [format!("convs{l}.{i}"), format!("tconvs{l}.{i}")])
            .chain([format!("up{l}")])
    });
    per_level.chain(["bottom.0", "bottom.2", "output_module.0"].map(String::from))
}

/// Where the models are: `RAWVIBES_MODELS`, then the install's
/// `share/rawvibes/models`, then this checkout's `models` (development).
pub fn models_dir() -> Option<PathBuf> {
    let installed =
        std::env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.join("share/rawvibes/models")));
    let dev = Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../models"));
    std::env::var_os("RAWVIBES_MODELS")
        .map(PathBuf::from)
        .into_iter()
        .chain(installed)
        .chain(dev)
        .map(|d| d.join("rawnind"))
        .find(|d| d.join("bayer.bin").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 18 3×3 convolutions, 4 transposed 2×2, one 1×1 head: weight + bias each,
    /// channels chaining 32 → 512 → 32, about 7.76 M parameters.
    #[test]
    fn weights_load_both_variants() {
        let Some(dir) = models_dir() else { return eprintln!("no models, skipped") };
        for (variant, inputs, head) in [(Variant::Bayer, 4, 12), (Variant::Linear, 3, 3)] {
            let w = Weights::load(&dir, variant).unwrap();
            assert_eq!(w.tensors.len(), 46, "{variant:?}");
            let kernels: Vec<&Tensor> = w.tensors.iter().step_by(2).collect();
            assert_eq!(kernels.iter().filter(|t| t.shape[2..] == [3, 3]).count(), 18);
            assert_eq!(kernels.iter().filter(|t| t.shape[2..] == [2, 2]).count(), 4);
            assert_eq!(kernels[0].shape[..2], [32, inputs]);
            assert_eq!(kernels.last().unwrap().shape, [head, 32, 1, 1]);
            let params: usize = w.tensors.iter().map(Tensor::size).sum();
            assert!((params as f32 / 7.76e6 - 1.0).abs() < 0.01, "{params}");
        }
    }
}
