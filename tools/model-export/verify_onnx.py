import sys, time, numpy as np, onnxruntime as ort, torch
from demucs.pretrained import get_model
name = sys.argv[1]
m = get_model(name); sub = m.models[0] if hasattr(m, "models") else m
mix = np.load(f"../../models/{name}_ref_mix.npy"); spec = np.load(f"../../models/{name}_ref_spec.npy")
ref = np.load(f"../../models/{name}_ref_out.npy")
for prov in [["CPUExecutionProvider"], ["CoreMLExecutionProvider","CPUExecutionProvider"]]:
    s = ort.InferenceSession(f"../../models/{name}.onnx", providers=prov)
    t=time.time(); xs, xt = s.run(None, {"mix": mix, "spec": spec}); t1=time.time()-t
    t=time.time(); s.run(None, {"mix": mix, "spec": spec}); t2=time.time()-t
    z = sub._spec(torch.from_numpy(mix))
    out = sub._ispec(sub._mask(z, torch.from_numpy(xs)), mix.shape[-1]).numpy() + xt
    err = np.abs(out-ref).max(); sdr = 10*np.log10((ref**2).sum()/((out-ref)**2).sum())
    print(prov[0], f"first {t1:.2f}s second {t2:.2f}s maxerr {err:.2e} SDR-vs-torch {sdr:.1f} dB")
