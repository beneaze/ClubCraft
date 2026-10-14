"""Export COCOLA (Ciranni et al., ICASSP 2025; MIT, github.com/gladia-research-group/cocola) for
ClubCraft's mashup re-ranking.

COCOLA scores how well two audio clips fit together harmonically / rhythmically. It was trained
on sub-mixes of separated stems (MoisesDB, Slakh2100, CocoChorales), which matches the question
"do these vocals fit this instrumental". MoisesDB is licensed for non-commercial research.

Outputs (in --out):
  cocola.onnx       embedding network: features [B, 2, 64, 501] -> embeddings [B, 512]
                    (EfficientNet-B0 -> linear -> LayerNorm -> tanh)
  cocola_w.bin      bilinear similarity matrix W (512 x 512 f32 LE, row-major): score = x . W y
  cocola.json       manifest (front end parameters)
  cocola_ref_*.npy  reference input / features / embeddings for the Rust tests

The front end (16 kHz mono 5 s clip -> STFT -> librosa HPSS -> mel -> dB) is reimplemented in
Rust (crates/analysis/src/cocola.rs) and checked against cocola_ref_*.npy.

usage: .venv/bin/python export_cocola.py --ckpt cocola_hp_v1.ckpt --out ../../models
The checkpoint is a Lightning pickle; it is loaded with torch.load(weights_only=True), with the
enum types it references stubbed, so loading cannot execute code.
"""

import argparse
import enum
import json
import os
import sys
import types

import librosa
import numpy as np
import torch
from torch import nn
from efficientnet_pytorch import EfficientNet

SR, N_FFT, WIN, HOP, FMIN, FMAX, N_MELS, SECONDS = 16000, 1024, 400, 160, 60.0, 7800.0, 64, 5
FRAMES = SR * SECONDS // HOP + 1  # 501


def load_state(ckpt):
    # stub the enums the hyper-parameters pickle refers to, then load with the safe unpickler
    mod = types.ModuleType("contrastive_model.constants")

    class EmbeddingMode(enum.Enum):
        HARMONIC = "harmonic"
        PERCUSSIVE = "percussive"
        BOTH = "both"
        RANDOM = "random"

    class ModelInputType(enum.Enum):
        SINGLE_CHANNEL_MEL_SPECTROGRAM = "single_channel_mel_spectrogram"
        DOUBLE_CHANNEL_HARMONIC_PERCUSSIVE = "double_channel_harmonic_percussive"

    class Dataset(enum.Enum):
        CCS = "coco_chorales_contrastive/*"
        CCS_RANDOM = "coco_chorales_contrastive/random"
        CCS_STRING = "coco_chorales_contrastive/string"
        CCS_BRASS = "coco_chorales_contrastive/brass"
        CCS_WOODWIND = "coco_chorales_contrastive/woodwind"
        SLAKH2100 = "slakh2100_contrastive"
        MOISESDB = "moisesdb_contrastive"
        MIXED = "mixed_contrastive"

    class ModelFeatureExtractorType(enum.Enum):
        HPSS = "hpss"
        MEL_SPECTROGRAM = "mel_spectrogram"

    class FeatureExtractionTime(enum.Enum):
        OFFLINE = "offline"
        ONLINE = "online"

    enums = (EmbeddingMode, ModelInputType, Dataset, ModelFeatureExtractorType, FeatureExtractionTime)
    for c in enums:
        c.__module__ = mod.__name__
        setattr(mod, c.__name__, c)
    pkg = types.ModuleType("contrastive_model")
    pkg.constants = mod
    sys.modules["contrastive_model"] = pkg
    sys.modules["contrastive_model.constants"] = mod
    torch.serialization.add_safe_globals(list(enums))
    ck = torch.load(ckpt, map_location="cpu", weights_only=True)
    return ck["state_dict"], ck.get("hyper_parameters", {})


class Embed(nn.Module):
    def __init__(self):
        super().__init__()
        self.net = EfficientNet.from_name("efficientnet-b0", include_top=False, in_channels=2)
        self.net.set_swish(memory_efficient=False)
        self.projection = nn.Linear(1280, 512)
        self.layer_norm = nn.LayerNorm(512)

    def forward(self, x):
        e = self.net(x).flatten(1)
        return torch.tanh(self.layer_norm(self.projection(e)))


def features(audio):
    """The COCOLA front end (feature_extraction.HPSS), for one 1-D clip."""
    stft = librosa.stft(audio, n_fft=N_FFT, win_length=WIN, hop_length=HOP)
    h, p = librosa.decompose.hpss(stft)
    out = []
    for s in (h, p):
        mel = librosa.feature.melspectrogram(S=np.abs(s) ** 2, sr=SR, fmin=FMIN, fmax=FMAX, n_mels=N_MELS)
        out.append(librosa.power_to_db(mel, ref=np.max))
    return np.stack(out, 0).astype(np.float32)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    sd, hp = load_state(a.ckpt)
    print("hyper-parameters:", {k: str(v) for k, v in hp.items()})
    m = Embed().eval()
    mapping = {}
    for k, v in sd.items():
        if k.startswith("encoder.encoder.model.0."):
            mapping["net." + k[len("encoder.encoder.model.0."):]] = v
        elif k.startswith("encoder.projection."):
            mapping["projection." + k[len("encoder.projection."):]] = v
        elif k.startswith("layer_norm."):
            mapping[k] = v
    missing, unexpected = m.load_state_dict(mapping, strict=True), None
    w = sd["similarity.w"].numpy().astype("<f4")
    assert w.shape == (512, 512)
    w.tofile(os.path.join(a.out, "cocola_w.bin"))

    # reference: two related clips (a chord and a melody over it) and an unrelated one
    t = np.arange(SR * SECONDS) / SR
    rng = np.random.default_rng(0)
    chord = sum(np.sin(2 * np.pi * f * t) for f in (220.0, 277.18, 329.63)) * 0.2
    melody = np.sin(2 * np.pi * np.where((t % 1.0) < 0.5, 440.0, 554.37) * t) * 0.3
    clash = np.sin(2 * np.pi * 233.08 * t) * 0.3 + rng.normal(0, 0.02, t.shape)
    clips = np.stack([chord, melody, clash]).astype(np.float32)
    feats = np.stack([features(c) for c in clips])
    assert feats.shape[1:] == (2, N_MELS, FRAMES), feats.shape
    with torch.no_grad():
        emb = m(torch.from_numpy(feats)).numpy()
    wt = torch.from_numpy(w)
    s = lambda i, j: float(torch.from_numpy(emb[i]) @ wt @ torch.from_numpy(emb[j]))
    print(f"score chord/melody {s(0, 1):.3f}  chord/clash {s(0, 2):.3f}")
    np.save(os.path.join(a.out, "cocola_ref_audio.npy"), clips)
    np.save(os.path.join(a.out, "cocola_ref_features.npy"), feats)
    np.save(os.path.join(a.out, "cocola_ref_embeddings.npy"), emb)

    path = os.path.join(a.out, "cocola.onnx")
    torch.onnx.export(m, torch.from_numpy(feats[:1]), path, input_names=["features"], output_names=["embedding"],
                      dynamic_axes={"features": {0: "batch"}, "embedding": {0: "batch"}}, opset_version=17)
    import onnxruntime as ort
    sess = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
    out = sess.run(None, {"features": feats})[0]
    print("onnx max |diff|", float(np.abs(out - emb).max()))
    json.dump({"name": "cocola_hp_v1", "sample_rate": SR, "seconds": SECONDS, "n_fft": N_FFT, "win_length": WIN, "hop": HOP,
               "fmin": FMIN, "fmax": FMAX, "n_mels": N_MELS, "frames": FRAMES, "hpss_kernel": 31, "top_db": 80.0,
               "embedding_dim": 512, "licence": "MIT (code and weights; trained on MoisesDB/Slakh2100/CocoChorales)",
               "source": "https://github.com/gladia-research-group/cocola (COCOLA_HP_v1)"},
              open(os.path.join(a.out, "cocola.json"), "w"), indent=1)
    print("wrote", a.out)


if __name__ == "__main__":
    main()
