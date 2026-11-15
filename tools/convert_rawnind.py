#!/usr/bin/env python3
"""Convert darktable's RawNIND UtNet2 ONNX models (rawdenoise-nind.dtmodel)
into the flat weights RawValley loads (core/src/ai/weights.rs).

For each variant (bayer, linear) it writes:
  <variant>.bin   little-endian f32: every Conv/ConvTranspose weight then
                  bias, in graph order
  <variant>.json  [{"name", "shape", "offset"}] for those tensors
With --ref it also writes <variant>_ref.bin: a deterministic 1×C×64×64 input
followed by the onnxruntime output for it (tests compare the GPU against it).

Usage: convert_rawnind.py <dir with model_bayer.onnx, model_linear.onnx> <out dir> [--ref]
Needs: onnx, numpy (and onnxruntime for --ref).
"""
import json
import sys
from pathlib import Path

import numpy as np
import onnx
from onnx import numpy_helper


def convert(model_path: Path, out: Path, variant: str, ref: bool) -> None:
    model = onnx.load(str(model_path))
    init = {t.name: numpy_helper.to_array(t).astype("<f4") for t in model.graph.initializer}
    tensors, blobs, offset = [], [], 0
    for node in model.graph.node:
        if node.op_type not in ("Conv", "ConvTranspose"):
            continue
        for name in node.input[1:]:
            a = init[name]
            tensors.append({"name": name, "shape": list(a.shape), "offset": offset})
            blobs.append(a.ravel())
            offset += a.size
    np.concatenate(blobs).tofile(out / f"{variant}.bin")
    (out / f"{variant}.json").write_text(json.dumps(tensors, indent=1))
    print(f"{variant}: {len(tensors)} tensors, {offset} floats")
    if ref:
        import onnxruntime as ort

        # The exported graph is fixed at 512: free the spatial dims.
        for v in list(model.graph.input) + list(model.graph.output):
            for d in v.type.tensor_type.shape.dim[2:]:
                d.ClearField("dim_value")
                d.dim_param = "s"
        channels = 4 if variant == "bayer" else 3
        rng = np.random.default_rng(7)
        x = (0.05 + 0.3 * rng.random((1, channels, 64, 64))).astype("<f4")
        sess = ort.InferenceSession(model.SerializeToString(), providers=["CPUExecutionProvider"])
        y = sess.run(None, {"input": x})[0].astype("<f4")
        np.concatenate([x.ravel(), y.ravel()]).tofile(out / f"{variant}_ref.bin")
        print(f"{variant}_ref: in {x.shape} out {y.shape}")


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for variant in ("bayer", "linear"):
        convert(src / f"model_{variant}.onnx", out, variant, "--ref" in sys.argv[3:])


if __name__ == "__main__":
    main()
