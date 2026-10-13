"""Run Python Demucs on exactly the signal the Rust example separated, with the same shifts,
and report per-stem SDR of the Rust output against it.

    cargo run --release -p clubcraft-stems --example separate -- track.mp3 high OUT --dump-input
    tools/model-export/.venv/bin/python crates/stems/tools/demucs_reference.py OUT --model htdemucs_ft \
        --shift-samples 11025 [--overlap 0.25] [--device cpu]

Shifts: the Rust pass `i` of `n` delays by `(2i+1)/(2n) * 22050` samples (Fast/High: 11025;
Ultra: 5512,16537); Demucs' random offset is `22050 - shift`, so `random.randint` is patched.
"""
import argparse
import os
import random
import time

import numpy as np
import torch
from demucs.apply import apply_model, BagOfModels
from demucs.pretrained import get_model


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--model", default="htdemucs_ft")
    ap.add_argument("--shift-samples", default="11025")
    ap.add_argument("--overlap", type=float, default=0.25)
    ap.add_argument("--device", default="cpu")
    args = ap.parse_args()
    torch.backends.mha.set_fastpath_enabled(False)

    mix = np.load(os.path.join(args.dir, "input.npy"))
    shifts = [int(s) for s in args.shift_samples.split(",") if s != ""]
    offsets = [22050 - s for s in shifts]
    model = get_model(args.model)
    n_sub = len(model.models) if isinstance(model, BagOfModels) else 1
    seq = iter(offsets * n_sub)
    random.randint = lambda a, b: next(seq)

    wav = torch.from_numpy(mix)
    ref = wav.mean(0)
    wav = (wav - ref.mean()) / ref.std()
    t = time.time()
    with torch.no_grad():
        out = apply_model(model, wav[None], shifts=len(shifts), split=True, overlap=args.overlap,
                          device=args.device, progress=False)[0]
    dt = time.time() - t
    out = out * ref.std() + ref.mean()
    out = out.cpu().numpy()
    np.save(os.path.join(args.dir, f"demucs_{args.model}.npy"), out.astype(np.float32))
    print(f"python demucs {args.model} on {args.device}: {dt:.1f} s for {mix.shape[1] / 44100:.1f} s audio")

    import soundfile as sf
    names = list(model.sources)
    for i, name in enumerate(names[:4] if len(names) == 4 else names):
        p = os.path.join(args.dir, f"{name}.wav")
        if not os.path.exists(p):
            continue
        rust, _ = sf.read(p, dtype="float32")
        rust = rust.T
        refs = out[i]
        if len(names) == 6 and name == "other":
            refs = out[2] + out[4] + out[5]
        num = (refs.astype(np.float64) ** 2).sum()
        den = ((refs.astype(np.float64) - rust) ** 2).sum()
        print(f"  {name:7s} SDR(rust vs python) = {10 * np.log10(num / max(den, 1e-30)):.1f} dB   max|diff| = {np.abs(refs - rust).max():.2e}")


if __name__ == "__main__":
    main()
