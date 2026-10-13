"""Export Demucs v4 (HTDemucs) models to ONNX for ClubCraft.

The STFT / iSTFT are not part of the exported graph (ONNX has no iSTFT and the
complex ops export poorly). Instead the graph takes the mixture waveform *and*
its complex-as-channels spectrogram, and returns the frequency-branch spectrogram
plus the time-branch waveform. The Rust side (crates/stems) performs the STFT,
the iSTFT and the final sum, mirroring `HTDemucs._spec` / `_ispec` exactly.

Usage:
    .venv/bin/python export_htdemucs.py --model htdemucs_ft --out ../../models
"""
import argparse
import json
import math
import os

import numpy as np
import torch
import torch.nn.functional as F
from demucs.pretrained import get_model
from demucs.apply import BagOfModels
from demucs.htdemucs import HTDemucs
from einops import rearrange


class Core(torch.nn.Module):
    """HTDemucs.forward without the spectral transforms."""

    def __init__(self, m: HTDemucs):
        super().__init__()
        self.m = m

    def forward(self, mix, mag):
        m = self.m
        x = mag
        B, C, Fq, T = x.shape
        mean = x.mean(dim=(1, 2, 3), keepdim=True)
        std = x.std(dim=(1, 2, 3), keepdim=True)
        x = (x - mean) / (1e-5 + std)

        xt = mix
        meant = xt.mean(dim=(1, 2), keepdim=True)
        stdt = xt.std(dim=(1, 2), keepdim=True)
        xt = (xt - meant) / (1e-5 + stdt)

        saved, saved_t, lengths, lengths_t = [], [], [], []
        for idx, encode in enumerate(m.encoder):
            lengths.append(x.shape[-1])
            inject = None
            if idx < len(m.tencoder):
                lengths_t.append(xt.shape[-1])
                tenc = m.tencoder[idx]
                xt = tenc(xt)
                if not tenc.empty:
                    saved_t.append(xt)
                else:
                    inject = xt
            x = encode(x, inject)
            if idx == 0 and m.freq_emb is not None:
                frs = torch.arange(x.shape[-2], device=x.device)
                emb = m.freq_emb(frs).t()[None, :, :, None].expand_as(x)
                x = x + m.freq_emb_scale * emb
            saved.append(x)

        if m.crosstransformer:
            if m.bottom_channels:
                b, c, f, t = x.shape
                x = rearrange(x, "b c f t-> b c (f t)")
                x = m.channel_upsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = m.channel_upsampler_t(xt)
            x, xt = m.crosstransformer(x, xt)
            if m.bottom_channels:
                x = rearrange(x, "b c f t-> b c (f t)")
                x = m.channel_downsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = m.channel_downsampler_t(xt)

        for idx, decode in enumerate(m.decoder):
            skip = saved.pop(-1)
            x, pre = decode(x, skip, lengths.pop(-1))
            offset = m.depth - len(m.tdecoder)
            if idx >= offset:
                tdec = m.tdecoder[idx - offset]
                length_t = lengths_t.pop(-1)
                if tdec.empty:
                    pre = pre[:, :, 0]
                    xt, _ = tdec(pre, None, length_t)
                else:
                    skip = saved_t.pop(-1)
                    xt, _ = tdec(xt, skip, length_t)

        S = len(m.sources)
        x = x.view(B, S, -1, Fq, T)
        x = x * std[:, None] + mean[:, None]
        xt = xt.view(B, S, -1, mix.shape[-1])
        xt = xt * stdt[:, None] + meant[:, None]
        return x, xt


def check_against_torch(m: HTDemucs, core: Core, L: int):
    """Sanity check: Core + torch spectral transforms == HTDemucs.forward."""
    torch.manual_seed(0)
    mix = torch.randn(1, 2, L) * 0.1
    with torch.no_grad():
        ref = m(mix)
        z = m._spec(mix)
        mag = m._magnitude(z)
        xs, xt = core(mix, mag)
        zout = m._mask(z, xs)
        out = m._ispec(zout, L) + xt
    err = (ref - out).abs().max().item()
    print(f"  core vs full forward max abs err: {err:.3e}")
    assert err < 1e-4


def export_coreml(m: HTDemucs, path: str):
    """CoreML ML Program (fp32, GPU). Compiled to .mlmodelc with `xcrun coremlcompiler`."""
    import subprocess
    import coremltools as ct
    core = Core(m).eval()
    L = int(m.segment * m.samplerate)
    T = int(math.ceil(L / m.hop_length))
    mix = torch.zeros(1, 2, L)
    mag = torch.zeros(1, 4, m.nfft // 2, T)
    with torch.no_grad():
        traced = torch.jit.trace(core, (mix, mag), check_trace=False)
    ml = ct.convert(
        traced,
        inputs=[ct.TensorType(name="mix", shape=mix.shape), ct.TensorType(name="spec", shape=mag.shape)],
        outputs=[ct.TensorType(name="spec_out"), ct.TensorType(name="wave_out")],
        convert_to="mlprogram", compute_precision=ct.precision.FLOAT32,
        compute_units=ct.ComputeUnit.CPU_AND_GPU, minimum_deployment_target=ct.target.macOS13,
    )
    ml.save(path)
    out_dir = os.path.dirname(path)
    subprocess.run(["xcrun", "coremlcompiler", "compile", path, out_dir], check=True,
                   stdout=subprocess.DEVNULL)


def export_one(m: HTDemucs, path: str, opset: int):
    m.eval()
    core = Core(m).eval()
    L = int(m.segment * m.samplerate)
    T = int(math.ceil(L / m.hop_length))
    check_against_torch(m, core, L)
    mix = torch.zeros(1, 2, L)
    mag = torch.zeros(1, 4, m.nfft // 2, T)
    torch.onnx.export(
        core, (mix, mag), path,
        input_names=["mix", "spec"], output_names=["spec_out", "wave_out"],
        opset_version=opset, do_constant_folding=True, dynamo=False,
    )
    return L, T


def write_reference(m: HTDemucs, core: Core, out_dir: str, tag: str):
    """Small reference vectors so the Rust STFT + ORT pipeline can be unit tested."""
    L = int(m.segment * m.samplerate)
    torch.manual_seed(1)
    t = torch.arange(L) / m.samplerate
    mix = torch.stack([torch.sin(2 * math.pi * 220 * t), torch.sin(2 * math.pi * 331 * t)])[None] * 0.3
    mix = mix + torch.randn(1, 2, L) * 0.01
    with torch.no_grad():
        z = m._spec(mix)
        mag = m._magnitude(z)
        full = m(mix)
    np.save(os.path.join(out_dir, f"{tag}_ref_mix.npy"), mix.numpy().astype(np.float32))
    np.save(os.path.join(out_dir, f"{tag}_ref_spec.npy"), mag.numpy().astype(np.float32))
    np.save(os.path.join(out_dir, f"{tag}_ref_out.npy"), full.numpy().astype(np.float32))


def main():
    # The fused fast path of nn.MultiheadAttention cannot be traced for CoreML.
    torch.backends.mha.set_fastpath_enabled(False)
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="htdemucs_ft")
    ap.add_argument("--out", default="../../models")
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--reference", action="store_true")
    ap.add_argument("--no-onnx", action="store_true")
    ap.add_argument("--no-coreml", action="store_true")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    model = get_model(args.model)
    subs = model.models if isinstance(model, BagOfModels) else [model]
    weights = model.weights if isinstance(model, BagOfModels) else [[1.0] * len(model.sources)]
    files = []
    for i, sub in enumerate(subs):
        assert isinstance(sub, HTDemucs), type(sub)
        name = f"{args.model}_{i}.onnx" if len(subs) > 1 else f"{args.model}.onnx"
        path = os.path.join(args.out, name)
        print(f"exporting {args.model}[{i}] -> {path}")
        sub.eval()
        L = int(sub.segment * sub.samplerate)
        T = int(math.ceil(L / sub.hop_length))
        if not args.no_onnx:
            export_one(sub, path, args.opset)
        if not args.no_coreml:
            export_coreml(sub, path.replace(".onnx", ".mlpackage"))
        files.append(name.replace(".onnx", ""))
        if args.reference and i == 0:
            write_reference(sub, Core(sub).eval(), args.out, args.model)

    first = subs[0]
    manifest = {
        "name": args.model,
        "architecture": "htdemucs",
        "sources": list(first.sources),
        "samplerate": first.samplerate,
        "audio_channels": first.audio_channels,
        "segment_samples": L,
        "spec_frames": T,
        "nfft": first.nfft,
        "hop_length": first.hop_length,
        "models": files,  # base names; <name>.onnx / <name>.mlmodelc
        # weights[i][s]: contribution of model i to source s (BagOfModels semantics)
        "weights": [list(map(float, w)) for w in weights],
    }
    with open(os.path.join(args.out, f"{args.model}.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
