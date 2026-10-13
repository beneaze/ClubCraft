"""Export Kimberley Jensen's Mel-Band RoFormer vocal model for ClubCraft.

Checkpoint: https://huggingface.co/KimberleyJSN/melbandroformer (MIT), `MelBandRoformer.ckpt`.
Model code: ZFTurbo/Music-Source-Separation-Training (MIT), vendored in `mel_band_roformer/`.
Config: `config_vocals_mel_band_roformer.yaml` of the checkpoint (reproduced in CONFIG below).

As for HTDemucs, the spectral transforms stay out of the graph (the Rust side does them exactly):

    x    [1, T, D]  band-split input: STFT bins gathered with `freq_indices` (stereo interleaved:
                    index = 2 * bin + channel), complex folded last: x[t, 2*k + c] = stft[idx[k], t, c]
    mask [1, T, D]  complex masks for the same gathered bins (same layout)

The Rust side then scatter-adds the masks back to the full spectrum, divides by the number of bands
covering each bin, multiplies with the mixture STFT and runs the iSTFT. T = 801 frames for the 8 s
(352800 samples) training chunk; D = 2 * len(freq_indices).

Usage (from tools/model-export):
    .venv/bin/python export_melroformer.py --out ../../models [--reference] [--fp16]
"""
import argparse
import json
import os
import subprocess
import urllib.request

import numpy as np
import torch

from mel_band_roformer import MelBandRoformer

CKPT_URL = "https://huggingface.co/KimberleyJSN/melbandroformer/resolve/main/MelBandRoformer.ckpt"

CONFIG = dict(
    dim=384, depth=6, stereo=True, num_stems=1, time_transformer_depth=1, freq_transformer_depth=1,
    num_bands=60, dim_head=64, heads=8, attn_dropout=0.0, ff_dropout=0.0,
    flash_attn=False,  # plain einsum attention: traces and converts cleanly
    dim_freqs_in=1025, sample_rate=44100, stft_n_fft=2048, stft_hop_length=441, stft_win_length=2048,
    stft_normalized=False, mask_estimator_depth=2,
    zero_dc=False,  # the original (Kim) implementation does not zero the DC bin
)
CHUNK = 352800
NUM_OVERLAP = 2


class Core(torch.nn.Module):
    """MelBandRoformer.forward between the band gather and the mask scatter."""

    def __init__(self, m: MelBandRoformer):
        super().__init__()
        self.m = m

    def forward(self, x):
        from einops import rearrange, pack, unpack
        m = self.m
        x = m.band_split(x)
        for block in m.layers:
            time_transformer, freq_transformer = block[-2], block[-1]
            x = rearrange(x, "b t f d -> b f t d")
            x, ps = pack([x], "* t d")
            x = time_transformer(x)
            x, = unpack(x, ps, "* t d")
            x = rearrange(x, "b f t d -> b t f d")
            x, ps = pack([x], "* f d")
            x = freq_transformer(x)
            x, = unpack(x, ps, "* f d")
        return m.mask_estimators[0](x)


def load_model(ckpt_path):
    if not os.path.exists(ckpt_path):
        os.makedirs(os.path.dirname(ckpt_path), exist_ok=True)
        print(f"downloading {CKPT_URL}")
        urllib.request.urlretrieve(CKPT_URL, ckpt_path)
    m = MelBandRoformer(**CONFIG).eval()
    sd = torch.load(ckpt_path, map_location="cpu", weights_only=True)
    if "state_dict" in sd:
        sd = sd["state_dict"]
    m.load_state_dict(sd, strict=True)
    return m


def stft(m, audio):
    """[B, 2, L] -> [B, 2*F (f s interleaved), T, 2] exactly like MelBandRoformer.forward."""
    from einops import rearrange
    b, s, l = audio.shape
    z = torch.stft(audio.reshape(b * s, l), **m.stft_kwargs, window=m.stft_window_fn(device=audio.device),
                   return_complex=True)
    z = torch.view_as_real(z).reshape(b, s, *z.shape[1:], 2)
    return rearrange(z, "b s f t c -> b (f s) t c")


def gather(m, z):
    from einops import rearrange
    x = z[:, m.freq_indices]
    return rearrange(x, "b f t c -> b t (f c)")


def check(m, core, L):
    torch.manual_seed(0)
    audio = torch.randn(1, 2, L) * 0.1
    with torch.no_grad():
        ref = m(audio)
        x = gather(m, stft(m, audio))
        mask = core(x)
        # replicate the tail of forward with the core's mask
        out = finish(m, audio, mask)
    err = (ref - out).abs().max().item()
    print(f"  core vs full forward max abs err: {err:.3e}")
    assert err < 1e-4


def finish(m, audio, mask):
    from einops import rearrange, repeat
    z = stft(m, audio)  # [1, 2F, T, 2]
    zc = torch.view_as_complex(z.contiguous())
    mc = torch.view_as_complex(rearrange(mask, "b t (f c) -> b f t c", c=2).contiguous())
    summed = torch.zeros_like(zc).index_add_(1, m.freq_indices, mc)
    denom = repeat(m.num_bands_per_freq, "f -> (f r) 1", r=2)
    zc = zc * (summed / denom.clamp(min=1e-8))
    zc = rearrange(zc, "b (f s) t -> (b s) f t", s=2)
    y = torch.istft(zc, **m.stft_kwargs, window=m.stft_window_fn(device=audio.device), return_complex=False)
    return y.reshape(1, 2, -1)


def main():
    torch.backends.mha.set_fastpath_enabled(False)
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="../../models")
    ap.add_argument("--ckpt", default="cache/melroformer/MelBandRoformer.ckpt")
    ap.add_argument("--reference", action="store_true")
    ap.add_argument("--no-onnx", action="store_true")
    ap.add_argument("--no-coreml", action="store_true")
    ap.add_argument("--fp16", action="store_true", help="also export an fp16 CoreML variant")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)
    name = "melroformer"

    m = load_model(args.ckpt)
    core = Core(m).eval()
    L = CHUNK
    T = L // CONFIG["stft_hop_length"] + 1
    D = 2 * len(m.freq_indices)
    with torch.no_grad():
        m(torch.zeros(1, 2, L))  # warm the rotary caches so the trace takes the cached path
    check(m, core, L)

    x = torch.zeros(1, T, D)
    if not args.no_onnx:
        path = os.path.join(args.out, f"{name}.onnx")
        print(f"exporting {path}")
        torch.onnx.export(core, (x,), path, input_names=["x"], output_names=["mask"], opset_version=17,
                          do_constant_folding=True, dynamo=False)

    if not args.no_coreml:
        import coremltools as ct
        with torch.no_grad():
            traced = torch.jit.trace(core, (x,), check_trace=False)
        variants = [("", ct.precision.FLOAT32)] + ([("_fp16", ct.precision.FLOAT16)] if args.fp16 else [])
        for suffix, prec in variants:
            ml = ct.convert(
                traced, inputs=[ct.TensorType(name="x", shape=x.shape)], outputs=[ct.TensorType(name="mask")],
                convert_to="mlprogram", compute_precision=prec, compute_units=ct.ComputeUnit.CPU_AND_GPU,
                minimum_deployment_target=ct.target.macOS13,
            )
            pkg = os.path.join(args.out, f"{name}{suffix}.mlpackage")
            ml.save(pkg)
            subprocess.run(["xcrun", "coremlcompiler", "compile", pkg, args.out], check=True, stdout=subprocess.DEVNULL)
            print(f"wrote {pkg} (+ .mlmodelc)")

    if args.reference:
        torch.manual_seed(1)
        t = torch.arange(L) / 44100
        mix = torch.stack([torch.sin(2 * np.pi * 220 * t), torch.sin(2 * np.pi * 331 * t)])[None] * 0.3
        mix = mix + torch.randn(1, 2, L) * 0.01
        with torch.no_grad():
            xr = gather(m, stft(m, mix))
            mask = core(xr)
            out = m(mix)
        for tag, arr in [("mix", mix), ("x", xr), ("mask", mask), ("out", out)]:
            np.save(os.path.join(args.out, f"{name}_ref_{tag}.npy"), arr.numpy().astype(np.float32))

    manifest = {
        "name": name,
        "architecture": "mel_band_roformer",
        "sources": ["vocals"],
        "samplerate": 44100,
        "audio_channels": 2,
        "chunk_samples": L,
        "frames": T,
        "nfft": CONFIG["stft_n_fft"],
        "hop_length": CONFIG["stft_hop_length"],
        "win_length": CONFIG["stft_win_length"],
        "num_overlap": NUM_OVERLAP,
        "zero_dc": CONFIG["zero_dc"],
        "models": [name],
        # gathered bin k reads STFT row freq_indices[k] of the (f s)-interleaved spectrum
        "freq_indices": [int(v) for v in m.freq_indices.tolist()],
        "num_bands_per_freq": [int(v) for v in m.num_bands_per_freq.tolist()],
        "license": "MIT (checkpoint: Kimberley Jensen; code: ZFTurbo, lucidrains)",
    }
    with open(os.path.join(args.out, f"{name}.json"), "w") as f:
        json.dump(manifest, f)
    print(f"manifest: T={T} D={D} bins={len(m.freq_indices)}")


if __name__ == "__main__":
    main()
