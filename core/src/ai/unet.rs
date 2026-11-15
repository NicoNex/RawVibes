//! UtNet2 (`unet.wgsl`): a 4-level U-Net, 32 → 512 channels, LeakyReLU 0.2,
//! max-pool down, 2×2 transposed convolutions up, skip connections by
//! concatenation; the Bayer variant ends in a pixel shuffle to full size.

use std::collections::HashMap;

use super::{Variant, Weights};
use crate::gpu::Gpu;

/// `LayerParams` of `unet.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerParams {
    w: u32,
    h: u32,
    cin: u32,
    cout: u32,
    src_c0: u32,
    dst_c0: u32,
    k: u32,
    relu: u32,
    w_off: u32,
    b_off: u32,
    _pad: [u32; 2],
}

/// Channels of the four encoder levels (the bottom has twice the last).
const WIDTHS: [u32; 4] = [32, 64, 128, 256];

pub struct UNet {
    variant: Variant,
    /// Every weight and bias, as `Weights::data`.
    weights: wgpu::Buffer,
    /// (offset of weight, offset of bias) by tensor name stem, e.g. "convs1.0".
    layers: HashMap<String, (u32, u32)>,
    conv: wgpu::ComputePipeline,
    convt: wgpu::ComputePipeline,
    pool: wgpu::ComputePipeline,
    shuffle: wgpu::ComputePipeline,
}

/// A planar tensor on the GPU.
#[derive(Clone)]
struct T {
    buf: wgpu::Buffer,
    w: u32,
    h: u32,
}

/// One dispatch: pipeline, its parameters, source, destination, workgroup z.
struct Op<'a> {
    pipeline: &'a wgpu::ComputePipeline,
    params: LayerParams,
    src: T,
    dst: T,
    z: u32,
}

impl UNet {
    pub fn new(gpu: &Gpu, w: &Weights) -> Self {
        let pipeline = crate::gpu::shader(&gpu.device, "unet.wgsl", include_str!("unet.wgsl"));
        let layers = w
            .tensors
            .chunks_exact(2)
            .map(|pair| {
                (pair[0].name.trim_end_matches(".weight").to_string(), (pair[0].offset as u32, pair[1].offset as u32))
            })
            .collect();
        Self {
            variant: w.variant,
            weights: gpu.storage(bytemuck::cast_slice(&w.data)),
            layers,
            conv: pipeline("conv"),
            convt: pipeline("convt"),
            pool: pipeline("pool"),
            shuffle: pipeline("shuffle"),
        }
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// A dispatch of `pipeline` from `src` (channels from `src_c0`) into
    /// `dst` (from `dst_c0`), with `name`'s weights when it has any.
    #[allow(clippy::too_many_arguments)]
    fn op<'a>(
        &'a self,
        ops: &mut Vec<Op<'a>>,
        pipeline: &'a wgpu::ComputePipeline,
        name: Option<&str>,
        (src, src_c0, cin): (&T, u32, u32),
        (dst, dst_c0, cout): (&T, u32, u32),
        k: u32,
    ) {
        let (w_off, b_off) = name.map_or((0, 0), |n| self.layers[n]);
        let relu = (k == 3) as u32;
        let params = LayerParams { w: dst.w, h: dst.h, cin, cout, src_c0, dst_c0, k, relu, w_off, b_off, _pad: [0; 2] };
        // Weighted layers (k > 0) cover 8 output channels per workgroup; pool and shuffle one.
        let z = if k > 0 { cout.div_ceil(8) } else { cout };
        ops.push(Op { pipeline, params, src: src.clone(), dst: dst.clone(), z });
    }

    /// Runs the network on `input` (planar, 4 channels for Bayer, 3 for
    /// linear, `w` × `h`, both multiples of 16). The output is 3 planar
    /// channels at `w` × `h` (linear) or `2w` × `2h` (Bayer).
    // shortcut: allocates every intermediate per call; keep them per tile size once tiling runs it per tile.
    pub fn run(&self, gpu: &Gpu, input: &wgpu::Buffer, w: u32, h: u32) -> wgpu::Buffer {
        debug_assert!(w.is_multiple_of(16) && h.is_multiple_of(16), "UtNet2 needs multiples of 16, got {w}×{h}");
        let bayer = self.variant == Variant::Bayer;
        let alloc = |c: u32, w: u32, h: u32| T {
            buf: gpu.buffer(c as u64 * w as u64 * h as u64 * 4, wgpu::BufferUsages::COPY_SRC),
            w,
            h,
        };
        let at = |c: u32, l: u32| alloc(c, w >> l, h >> l);
        // cat[l]: the decoder's input at level l, up-sampled channels first, then the encoder's.
        let cat: Vec<T> = (0..4).map(|l| at(2 * WIDTHS[l as usize], l)).collect();
        let tmp: Vec<T> = (0..4).map(|l| at(WIDTHS[l as usize], l)).collect();
        let dec: Vec<T> = (0..4).map(|l| at(WIDTHS[l as usize], l)).collect();
        let pooled: Vec<T> = (0..4).map(|l| at(WIDTHS[l as usize], l + 1)).collect();
        let bottom = [at(512, 4), at(512, 4)];
        let heads = if bayer { 12 } else { 3 };
        let head = at(heads, 0);
        let out = if bayer { alloc(3, 2 * w, 2 * h) } else { head.clone() };

        let mut ops = Vec::new();
        let (conv, convt, pool) = (&self.conv, &self.convt, &self.pool);
        let mut src = (T { buf: input.clone(), w, h }, if bayer { 4 } else { 3 });
        for l in 0..4 {
            let c = WIDTHS[l];
            self.op(&mut ops, conv, Some(&format!("convs{}.0", l + 1)), (&src.0, 0, src.1), (&tmp[l], 0, c), 3);
            self.op(&mut ops, conv, Some(&format!("convs{}.2", l + 1)), (&tmp[l], 0, c), (&cat[l], c, c), 3);
            self.op(&mut ops, pool, None, (&cat[l], c, c), (&pooled[l], 0, c), 0);
            src = (pooled[l].clone(), c);
        }
        self.op(&mut ops, conv, Some("bottom.0"), (&pooled[3], 0, 256), (&bottom[0], 0, 512), 3);
        self.op(&mut ops, conv, Some("bottom.2"), (&bottom[0], 0, 512), (&bottom[1], 0, 512), 3);
        let mut src = (bottom[1].clone(), 512);
        for (i, l) in (0..4).rev().enumerate() {
            let c = WIDTHS[l];
            self.op(&mut ops, convt, Some(&format!("up{}", i + 1)), (&src.0, 0, src.1), (&cat[l], 0, c), 2);
            self.op(&mut ops, conv, Some(&format!("tconvs{}.0", i + 1)), (&cat[l], 0, 2 * c), (&tmp[l], 0, c), 3);
            self.op(&mut ops, conv, Some(&format!("tconvs{}.2", i + 1)), (&tmp[l], 0, c), (&dec[l], 0, c), 3);
            src = (dec[l].clone(), c);
        }
        self.op(&mut ops, conv, Some("output_module.0"), (&dec[0], 0, 32), (&head, 0, heads), 1);
        if bayer {
            self.op(&mut ops, &self.shuffle, None, (&head, 0, 12), (&out, 0, 3), 0);
        }
        self.dispatch(gpu, &ops);
        out.buf
    }

    /// Records `ops` in order and submits them (or adds them to the open batch).
    fn dispatch(&self, gpu: &Gpu, ops: &[Op]) {
        let binds: Vec<wgpu::BindGroup> = ops
            .iter()
            .map(|o| {
                let params = gpu.storage(bytemuck::bytes_of(&o.params));
                let all = [(0, &params), (1, &o.src.buf), (2, &o.dst.buf), (3, &self.weights)];
                // Layouts come from what each entry point uses: pool and shuffle read no weights.
                let used = if o.params.k == 0 { &all[..3] } else { &all[..] };
                gpu.bind(o.pipeline, used)
            })
            .collect();
        gpu.record(|enc| {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for (o, bind) in ops.iter().zip(&binds) {
                pass.set_pipeline(o.pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.dispatch_workgroups(o.params.w.div_ceil(16), o.params.h.div_ceil(16), o.z);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::models_dir;

    /// Our shaders reproduce onnxruntime on the reference tile, both variants.
    #[test]
    fn unet_matches_onnx_reference() {
        let Some(dir) = models_dir() else { return eprintln!("no models, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        for (variant, cin, scale) in [(Variant::Bayer, 4usize, 2usize), (Variant::Linear, 3, 1)] {
            let stem = if variant == Variant::Bayer { "bayer" } else { "linear" };
            // Written only by `convert_rawnind.py --ref` (needs onnxruntime).
            let Ok(bytes) = std::fs::read(dir.join(format!("{stem}_ref.bin"))) else {
                return eprintln!("no reference tile, skipped");
            };
            let all: Vec<f32> = bytemuck::pod_collect_to_vec(&bytes);
            let (input, reference) = all.split_at(cin * 64 * 64);
            assert_eq!(reference.len(), 3 * 64 * scale * 64 * scale);
            let net = UNet::new(&gpu, &Weights::load(&dir, variant).unwrap());
            let out = gpu.read_f32(&net.run(&gpu, &gpu.storage(bytemuck::cast_slice(input)), 64, 64));
            let top = reference.iter().fold(0f32, |m, v| m.max(v.abs()));
            let worst = out.iter().zip(reference).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(out.iter().all(|v| v.is_finite()), "{variant:?}: non-finite output");
            assert!(worst <= 1e-4 * top, "{variant:?}: max error {worst} of {top}");
        }
    }
}
