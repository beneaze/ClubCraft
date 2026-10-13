"""PyTorch reference for the Mel-Band RoFormer vocal pass: runs the checkpoint's reference
chunking (`demix_track`: 8 s chunks, num_overlap 2, linear fades, reflect padding) on the
`input.npy` written by `separate --dump-input`, and compares with the Rust `vocals.wav`
(for `--quality best` the vocal stem is exactly the RoFormer output).

    tools/model-export/.venv/bin/python crates/stems/tools/roformer_reference.py OUT_DIR
"""
import os
import sys
import time

import numpy as np
import soundfile as sf
import torch
import torch.nn as nn

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../../tools/model-export"))
from export_melroformer import load_model  # noqa: E402


def demix(model, mix, C=352800, N=2):
    step = C // N
    fade = C // 10
    border = C - step
    if mix.shape[1] > 2 * border and border > 0:
        mix = nn.functional.pad(mix, (border, border), mode="reflect")
    window = torch.ones(C)
    window[-fade:] *= torch.linspace(1, 0, fade)
    window[:fade] *= torch.linspace(0, 1, fade)
    total = mix.shape[1]
    result = torch.zeros((1,) + tuple(mix.shape))
    counter = torch.zeros((1,) + tuple(mix.shape))
    i = 0
    with torch.no_grad():
        while i < total:
            part = mix[:, i:i + C]
            length = part.shape[-1]
            if length < C:
                if length > C // 2 + 1:
                    part = nn.functional.pad(part, (0, C - length), mode="reflect")
                else:
                    part = nn.functional.pad(part, (0, C - length, 0, 0), mode="constant", value=0)
            x = model(part.unsqueeze(0))[0]
            w = window.clone()
            if i == 0:
                w[:fade] = 1
            elif i + C >= total:
                w[-fade:] = 1
            result[..., i:i + length] += x[..., :length] * w[..., :length]
            counter[..., i:i + length] += w[..., :length]
            i += step
    est = (result / counter).numpy()
    np.nan_to_num(est, copy=False, nan=0.0)
    if mix.shape[1] > 2 * border and border > 0:
        est = est[..., border:-border]
    return est[0]


def main():
    d = sys.argv[1]
    torch.backends.mha.set_fastpath_enabled(False)
    model = load_model(os.path.join(os.path.dirname(__file__), "../../../tools/model-export/cache/melroformer/MelBandRoformer.ckpt"))
    mix = torch.from_numpy(np.load(os.path.join(d, "input.npy")))
    t = time.time()
    voc = demix(model, mix)
    print(f"torch cpu: {time.time() - t:.1f} s for {mix.shape[1] / 44100:.1f} s")
    np.save(os.path.join(d, "roformer_vocals.npy"), voc.astype(np.float32))
    rust, _ = sf.read(os.path.join(d, "vocals.wav"), dtype="float32")
    rust = rust.T
    num = (voc.astype(np.float64) ** 2).sum()
    den = ((voc.astype(np.float64) - rust) ** 2).sum()
    print(f"vocals SDR(rust vs torch) = {10 * np.log10(num / den):.1f} dB, max|diff| = {np.abs(voc - rust).max():.2e}")


if __name__ == "__main__":
    main()
