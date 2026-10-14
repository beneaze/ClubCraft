"""All-In-One music structure model (Kim & Choi 2023, github.com/mir-aidj/all-in-one, MIT)
re-implemented without NATTEN so it can be exported to ONNX / CoreML for ClubCraft.

The network definition below is a condensed copy of `allin1.models` (MIT licence, (c) Taejun
Kim) with the NATTEN custom ops (`natten1dqkrpb/natten1dav/natten2dqkrpb/natten2dav`) replaced
by gather-based PyTorch code reproducing NATTEN 0.14's CPU kernels exactly (window start /
relative-position-bias start rules from `natten_cpu_commons.h`). Weights: the 8 `harmonix-fold*`
checkpoints from huggingface.co/taejunkim/allinone (put them in `cache/allin1/`).

Usage
  # reference activations for Demucs stem folders (bass/drums/other/vocals.{wav,mp3}):
  .venv/bin/python export_allin1.py ref --stems <dir-with-stem-folders> --out <npz-dir>
  # reference activations for spectrograms written by the Rust front end (<name>.spec.npy):
  .venv/bin/python export_allin1.py ref --specs <dir> --out <npz-dir>
  # export the 8-fold ensemble (logits averaged in-graph) to ONNX + CoreML:
  .venv/bin/python export_allin1.py export --out ../../models
"""

import argparse
import json
import math
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

HERE = Path(__file__).resolve().parent
CKPT_DIR = HERE / "cache" / "allin1"
FOLDS = [
    "harmonix-fold0-0vra4ys2", "harmonix-fold1-3ozjhtsj", "harmonix-fold2-gmgo0nsy", "harmonix-fold3-i92b7m8p",
    "harmonix-fold4-1bql5qo0", "harmonix-fold5-x4z5zeef", "harmonix-fold6-x7t226rq", "harmonix-fold7-qwwskhg6",
]
MIN_FRAMES = 10240
LABELS = ["start", "end", "intro", "outro", "break", "bridge", "inst", "solo", "verse", "chorus"]

# ----------------------------------------------------------------------------------------------
# Front end: madmom FramedSignal(2048, fps=100) -> STFT (np.hanning) -> LogarithmicFilterbank
# (12 bands/octave, 30..17000 Hz, norm, unique) -> log10(1 + x).  (The Rust port must match.)
# ----------------------------------------------------------------------------------------------

def log_filterbank(n_fft=2048, sr=44100, bands=12, fmin=30.0, fmax=17000.0, fref=440.0):
    bin_freqs = np.fft.fftfreq(n_fft, 1.0 / sr)[: n_fft // 2]
    left = np.floor(np.log2(float(fmin) / fref) * bands)
    right = np.ceil(np.log2(float(fmax) / fref) * bands)
    freqs = fref * 2.0 ** (np.arange(left, right) / float(bands))
    freqs = freqs[np.searchsorted(freqs, fmin):]
    freqs = freqs[: np.searchsorted(freqs, fmax, "right")]
    idx = bin_freqs.searchsorted(freqs)
    idx = np.clip(idx, 1, len(bin_freqs) - 1)
    lf = bin_freqs[idx - 1]
    rf = bin_freqs[idx]
    idx -= freqs - lf < rf - freqs
    bins = np.unique(idx)
    fb = np.zeros((len(bin_freqs), len(bins) - 2), dtype=np.float32)
    for b in range(len(bins) - 2):
        start, center, stop = int(bins[b]), int(bins[b + 1]), int(bins[b + 2])
        if stop - start < 2:
            center = start
            stop = start + 1
        data = np.zeros(stop - start)
        data[: center - start] = np.linspace(0, 1, center - start, endpoint=False)
        data[center - start:] = np.linspace(1, 0, stop - center, endpoint=False)
        data /= np.sum(data)
        fb[start:stop, b] = data
    return fb


def spectrogram(mono, n_fft=2048, hop=441):
    n = len(mono)
    frames = int(np.ceil(n / float(hop)))
    win = np.hanning(n_fft).astype(np.float32)
    pad = np.concatenate([np.zeros(n_fft // 2, np.float32), mono.astype(np.float32), np.zeros(n_fft, np.float32)])
    idx = np.arange(frames)[:, None] * hop + np.arange(n_fft)[None, :]
    fr = pad[idx] * win
    mag = np.abs(np.fft.rfft(fr, axis=1))[:, : n_fft // 2].astype(np.float32)
    fb = log_filterbank(n_fft)
    return np.log10(mag @ fb + 1.0).astype(np.float32)


def load_mono(path):
    import librosa
    y, sr = librosa.load(str(path), sr=None, mono=False)
    assert sr == 44100, sr
    if y.ndim == 2:
        y = y.mean(axis=0)
    return y.astype(np.float32)


def stem_spec(folder):
    specs = []
    for name in ["bass", "drums", "other", "vocals"]:
        f = next((folder / f"{name}.{e}" for e in ("wav", "flac", "mp3") if (folder / f"{name}.{e}").exists()), None)
        if f is None:
            raise FileNotFoundError(f"{folder}/{name}")
        specs.append(spectrogram(load_mono(f)))
    t = min(s.shape[0] for s in specs)
    return np.stack([s[:t] for s in specs])  # 4, T, 81


# ----------------------------------------------------------------------------------------------
# Neighbourhood attention without NATTEN (exact NATTEN 0.14 semantics, traceable, dynamic length)
# ----------------------------------------------------------------------------------------------

def _window(length, k, d):
    """Window start ni and rpb start pi for every index (NATTEN get_window_start / get_pb_start)."""
    i = torch.arange(length)
    nb = k // 2
    if d <= 1:
        ni = torch.clamp(i - nb, min=0) + (i + nb >= length).long() * (length - i - nb - 1)
        pi = nb + (i < nb).long() * (nb - i) + (i + nb >= length).long() * (length - i - 1 - nb)
        return ni, pi
    imodd = i % d
    a = torch.div(length, d, rounding_mode="floor") * d
    b = length - a
    end_ni = torch.where(imodd < b, length - b + imodd - 2 * nb * d, a + imodd - k * d)
    ni = torch.where(i - nb * d < 0, imodd, torch.where(i + nb * d >= length, end_ni, i - nb * d))
    pi = torch.where(
        i - nb * d < 0,
        k - 1 - torch.div(i, d, rounding_mode="floor"),
        torch.where(i + nb * d >= length, torch.div(length - i - 1, d, rounding_mode="floor"), torch.full_like(i, nb)),
    )
    return ni, pi


def na1d(q, k, v, rpb, ks, d):
    # q, k, v: B, H, L, D ; rpb: H, 2k-1
    L = q.shape[2]
    ni, pi = _window(L, ks, d)
    kk = torch.arange(ks)
    idx = ni[:, None] + kk[None, :] * d  # L, ks
    pidx = pi[:, None] + kk[None, :]  # L, ks
    kg = k[:, :, idx]  # B, H, L, ks, D
    attn = (q.unsqueeze(3) * kg).sum(-1) + rpb[:, pidx].unsqueeze(0)
    attn = attn.softmax(-1)
    return (attn.unsqueeze(-1) * v[:, :, idx]).sum(3)  # B, H, L, D


def na2d(q, k, v, rpb, ks, d):
    """2D NA. When the first axis has exactly `ks` rows and dilation 1 (always the case here:
    4 stems padded to 5, kernel 5) every query attends to all rows, so only the second axis
    needs a gather; scores / outputs are einsums (12x less memory than the generic gather)."""
    B, Hh, X, Y, D = q.shape
    if X == ks and d == 1:
        _, pi = _window(X, ks, d)
        nj, pj = _window(Y, ks, d)
        kk = torch.arange(ks)
        iy = nj[:, None] + kk[None, :]  # Y, ks
        rx = pi[:, None] + kk[None, :]  # X, ks (row offsets)
        ry = pj[:, None] + kk[None, :]  # Y, ks
        bias = rpb[:, rx[:, None, :, None], ry[None, :, None, :]]  # H, X, Y, ks(z), ks(w)
        kw = k[:, :, :, iy]  # B, H, Z, Y, W, D
        vw = v[:, :, :, iy]
        attn = torch.einsum("bhxyd,bhzywd->bhxyzw", q, kw) + bias.unsqueeze(0)
        attn = attn.reshape(B, Hh, X, Y, ks * ks).softmax(-1).reshape(B, Hh, X, Y, ks, ks)
        return torch.einsum("bhxyzw,bhzywd->bhxyd", attn, vw)
    return na2d_generic(q, k, v, rpb, ks, d)


def na2d_generic(q, k, v, rpb, ks, d):
    # q, k, v: B, H, X(height), Y(width), D ; rpb: H, 2k-1, 2k-1
    B, Hh, X, Y, D = q.shape
    ni, pi = _window(X, ks, d)
    nj, pj = _window(Y, ks, d)
    kk = torch.arange(ks)
    ix = ni[:, None] + kk[None, :] * d  # X, ks
    iy = nj[:, None] + kk[None, :] * d  # Y, ks
    lin = (ix[:, None, :, None] * Y + iy[None, :, None, :]).reshape(X, Y, ks * ks)  # X, Y, ks*ks
    rx = (pi[:, None] + kk[None, :])  # X, ks
    ry = (pj[:, None] + kk[None, :])  # Y, ks
    bias = rpb[:, rx[:, None, :, None], ry[None, :, None, :]].reshape(Hh, X, Y, ks * ks)
    kf = k.reshape(B, Hh, X * Y, D)
    vf = v.reshape(B, Hh, X * Y, D)
    kg = kf[:, :, lin]  # B, H, X, Y, ks*ks, D
    attn = (q.unsqueeze(4) * kg).sum(-1) + bias.unsqueeze(0)
    attn = attn.softmax(-1)
    return (attn.unsqueeze(-1) * vf[:, :, lin]).sum(4)  # B, H, X, Y, D


# ----------------------------------------------------------------------------------------------
# Model (condensed from allin1.models.allinone / dinat; inference only)
# ----------------------------------------------------------------------------------------------

class NA(nn.Module):
    def __init__(self, dim, heads, ks, d, two_d):
        super().__init__()
        self.h, self.hd, self.ks, self.d, self.two_d = heads, dim // heads, ks, d, two_d
        self.query = nn.Linear(dim, dim)
        self.key = nn.Linear(dim, dim)
        self.value = nn.Linear(dim, dim)
        shape = (heads, 2 * ks - 1, 2 * ks - 1) if two_d else (heads, 2 * ks - 1)
        self.rpb = nn.Parameter(torch.zeros(shape))

    def split(self, x):
        x = x.view(*x.shape[:-1], self.h, self.hd)
        return x.permute(0, 3, 1, 2, 4) if self.two_d else x.permute(0, 2, 1, 3)

    def forward(self, x):
        q = self.split(self.query(x)) / math.sqrt(self.hd)
        k = self.split(self.key(x))
        v = self.split(self.value(x))
        if self.two_d:
            c = na2d(q, k, v, self.rpb, self.ks, self.d).permute(0, 2, 3, 1, 4)
        else:
            c = na1d(q, k, v, self.rpb, self.ks, self.d).permute(0, 2, 1, 3)
        return c.reshape(*c.shape[:-2], self.h * self.hd)


class NAModule(nn.Module):
    def __init__(self, dim, heads, ks, d, two_d):
        super().__init__()
        self.self = NA(dim, heads, ks, d, two_d)
        self.output = nn.Module()
        self.output.dense = nn.Linear(dim, dim)

    def forward(self, x):
        return self.output.dense(self.self(x))


class Dinat(nn.Module):
    def __init__(self, dim, heads, ks, d, double, two_d, mlp_ratio, eps):
        super().__init__()
        self.two_d, self.double = two_d, double
        self.window = ks * d * (2 if double else 1)
        self.layernorm_before = nn.LayerNorm(dim, eps=eps)
        self.attention = NAModule(dim, heads, ks, d, two_d)
        self.attention2 = NAModule(dim, heads, ks, 2 * d, two_d) if double else None
        da = dim * 2 if double else dim
        self.layernorm_after = nn.LayerNorm(da, eps=eps)
        self.intermediate = nn.Module()
        self.intermediate.dense = nn.Linear(da, int(da * mlp_ratio))
        self.output = nn.Module()
        self.output.dense = nn.Linear(int(da * mlp_ratio), dim)

    def forward(self, x):
        shortcut = x
        h = self.layernorm_before(x)
        if self.two_d:
            _, K, T, _ = x.shape
            pad_t = max(0, self.window - T)
            pad_k = max(0, self.window - K)
            if pad_t or pad_k:
                h = F.pad(h, (0, 0, 0, pad_t, 0, pad_k))
        else:
            _, T, _ = x.shape
            pad_t = max(0, self.window - T)
            if pad_t:
                h = F.pad(h, (0, 0, 0, pad_t))
        outs = []
        for att in [self.attention, self.attention2]:
            if att is None:
                continue
            a = att(h)
            a = a[:, :K, :T] if self.two_d else a[:, :T]
            outs.append(shortcut + a)
        if self.double:
            hs = torch.cat(outs, dim=-1)
            shortcut = (outs[0] + outs[1]) / 2.0
        else:
            hs = outs[0]
            shortcut = hs
        y = self.output.dense(F.gelu(self.intermediate.dense(self.layernorm_after(hs))))
        return shortcut + y


class AllInOne(nn.Module):
    def __init__(self, cfg):
        super().__init__()
        dim, eps = cfg["dim_embed"], cfg["layer_norm_eps"]
        self.embeddings = nn.Module()
        e = self.embeddings
        e.conv0 = nn.Conv2d(1, dim // 2, (3, 3), padding=(1, 0))
        e.conv1 = nn.Conv2d(dim // 2, dim, (1, 12))
        e.conv2 = nn.Conv2d(dim, dim, (3, 3), padding=(1, 0))
        e.norm = nn.LayerNorm(dim)
        self.encoder = nn.Module()
        layers = []
        for i in range(cfg["depth"]):
            d = min(cfg["dilation_factor"] ** i, cfg["dilation_max"])
            blk = nn.Module()
            blk.timelayer = Dinat(dim, cfg["num_heads"], cfg["kernel_size"], d, cfg["double_attention"], False, cfg["mlp_ratio"], eps)
            blk.instlayer = Dinat(dim, cfg["num_heads"], 5, 1, False, True, cfg["mlp_ratio"], eps)
            layers.append(blk)
        self.encoder.layers = nn.ModuleList(layers)
        self.norm = nn.LayerNorm(dim, eps=eps)
        k = cfg["data"]["num_instruments"]
        for name, n in [("beat", 1), ("downbeat", 1), ("section", 1), ("function", cfg["data"]["num_labels"])]:
            head = nn.Module()
            head.classifier = nn.Linear(k * dim, n)
            setattr(self, f"{name}_classifier", head)

    def forward(self, x):  # x: 1, 4, T, 81
        N, K, T, Fq = x.shape
        e = self.embeddings
        h = x.reshape(N * K, 1, T, Fq)
        h = F.elu(F.max_pool2d(e.conv0(h), (1, 3)))
        h = F.elu(F.max_pool2d(e.conv1(h), (1, 3)))
        h = F.elu(F.max_pool2d(e.conv2(h), (1, 3)))
        h = e.norm(h.squeeze(-1).permute(0, 2, 1))  # NK, T, C
        for blk in self.encoder.layers:
            h = blk.timelayer(h)
            h = blk.instlayer(h.reshape(N, K, T, -1)).reshape(N * K, T, -1)
        h = self.norm(h.reshape(N, K, T, -1))
        z = h.permute(0, 2, 1, 3).reshape(N, T, -1)
        outs = [getattr(self, f"{n}_classifier").classifier(z) for n in ("beat", "downbeat", "section", "function")]
        return torch.cat(outs, dim=-1)  # N, T, 13 logits: beat, downbeat, section, 10 functions


class Ensemble(nn.Module):
    """Mean of the fold logits (like allin1.models.Ensemble), then sigmoid / softmax."""

    def __init__(self, models):
        super().__init__()
        self.models = nn.ModuleList(models)

    def forward(self, x):
        z = torch.stack([m(x) for m in self.models]).mean(0)  # 1, T, 13
        return torch.cat([torch.sigmoid(z[..., :3]), torch.softmax(z[..., 3:], dim=-1)], dim=-1)


def load_models(folds=FOLDS):
    models = []
    for f in folds:
        ck = torch.load(CKPT_DIR / f"{f}.pth", map_location="cpu", weights_only=False)
        m = AllInOne(ck["config"])
        missing, unexpected = m.load_state_dict(ck["state_dict"], strict=False)
        unexpected = [u for u in unexpected if "num_batches" not in u]
        if missing or unexpected:
            raise RuntimeError(f"state dict mismatch: missing {missing}, unexpected {unexpected}")
        models.append(m.eval())
    return models


def cmd_ref(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    ens = Ensemble(load_models()).eval()
    items = []
    if args.stems:
        for d in sorted(Path(args.stems).iterdir()):
            if d.is_dir():
                items.append((d.name, lambda d=d: stem_spec(d)))
    if args.specs:
        for f in sorted(Path(args.specs).glob("*.spec.npy")):
            items.append((f.name[: -len(".spec.npy")], lambda f=f: np.load(f)))
    for name, get in items:
        dst = out / f"{name}.act.npy"
        if dst.exists() and not args.force:
            continue
        spec = get()
        if args.stems:
            np.save(out / f"{name}.pyspec.npy", spec)
        with torch.no_grad():
            act = ens(torch.from_numpy(spec).unsqueeze(0))[0].numpy()  # T, 13
        np.save(dst, act.astype(np.float32))
        print(f"{name}: {spec.shape[1]} frames", flush=True)


def cmd_export(args):
    out = Path(args.out)
    ens = Ensemble(load_models()).eval()
    # Trace with T > the largest attention window (5 * 1024 * 2 = 10240 frames) so the short-input
    # padding branches are not taken; the runtime only uses the model for tracks >= MIN_FRAMES.
    x = torch.randn(1, 4, MIN_FRAMES + 500, 81).abs()
    onnx_path = out / "allin1.onnx"
    with torch.no_grad():
        torch.onnx.export(
            ens, (x,), str(onnx_path), input_names=["spec"], output_names=["act"],
            dynamic_axes={"spec": {2: "frames"}, "act": {1: "frames"}}, opset_version=17,
        )
    print("exported", onnx_path, flush=True)
    if args.no_check:
        return write_manifest(out)
    import onnxruntime as ort
    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    for T in (MIN_FRAMES, 30017):
        x = torch.randn(1, 4, T, 81).abs()
        with torch.no_grad():
            ref = ens(x).numpy()
        got = sess.run(None, {"spec": x.numpy()})[0]
        print(f"ONNX check T={T}: max abs diff {np.abs(ref - got).max():.2e}", flush=True)
    write_manifest(out)


def write_manifest(out):
    man = {
        "name": "allin1", "source": "mir-aidj/all-in-one harmonix-fold0..7 (MIT)", "input": "spec [1,4,T,81]",
        "stems": ["bass", "drums", "other", "vocals"], "fps": 100, "frame_size": 2048, "hop": 441,
        "bands_per_octave": 12, "fmin": 30, "fmax": 17000,
        "min_frames": MIN_FRAMES,
        "min_frames": MIN_FRAMES,
        "output": "act [1,T,13] = sigmoid(beat, downbeat, section), softmax(10 functions)", "labels": LABELS,
    }
    (out / "allin1.json").write_text(json.dumps(man, indent=2))


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("ref")
    r.add_argument("--stems")
    r.add_argument("--specs")
    r.add_argument("--out", required=True)
    r.add_argument("--force", action="store_true")
    e = sub.add_parser("export")
    e.add_argument("--out", required=True)
    e.add_argument("--no-check", action="store_true")
    a = p.parse_args()
    {"ref": cmd_ref, "export": cmd_export}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
