# AI denoise models

`rawnind/` holds the RawNIND UtNet2 denoisers RawValley runs on the GPU
(`core/src/ai/`): a Bayer variant (packed RGGB in, demosaiced camera RGB
out) and a linear Rec.2020 variant for other sensors.

- Authors: Benoit Brummer and Christophe De Vleeschouwer, "Learning Joint
  Denoising, Demosaicing, and Compression from the Raw Natural Image Noise
  Dataset", arXiv:2501.08924 (2025).
- Code and original checkpoints: https://github.com/trougnouf/rawnind_jddc
  (checkpoints linked from its README; Bayer iteration 4,350,000, linear
  iteration 3,900,000).
- Weights licence: dual GPL-3.0 and CC-BY-4.0 (rawnind_jddc README).
  Training images: CC BY 4.0 / CC0 (Wikimedia Commons).
- RawValley takes the ONNX export darktable ships (`rawdenoise-nind`,
  https://github.com/darktable-org/darktable-ai, release-5.6.0) and
  converts it with `tools/convert_rawnind.py` (`make models`) into flat
  f32 weights plus a tensor list. That script, with the sources above, is
  how these files are rebuilt.
