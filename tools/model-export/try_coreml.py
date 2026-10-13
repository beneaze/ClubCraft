import time, math, numpy as np, torch, coremltools as ct
from demucs.pretrained import get_model
from export_htdemucs import Core
torch.backends.mha.set_fastpath_enabled(False)
m = get_model("htdemucs"); m = m.models[0] if hasattr(m, "models") else m; m.eval()
core = Core(m).eval()
mix = torch.from_numpy(np.load("../../models/htdemucs_ref_mix.npy")); spec = torch.from_numpy(np.load("../../models/htdemucs_ref_spec.npy"))
with torch.no_grad():
    ref = core(mix, spec)
    tr = torch.jit.trace(core, (mix, spec), check_trace=False)
for prec, cu in [(ct.precision.FLOAT32, ct.ComputeUnit.CPU_AND_GPU), (ct.precision.FLOAT16, ct.ComputeUnit.CPU_AND_GPU), (ct.precision.FLOAT16, ct.ComputeUnit.ALL)]:
    t=time.time()
    ml = ct.convert(tr, inputs=[ct.TensorType(name="mix", shape=mix.shape), ct.TensorType(name="spec", shape=spec.shape)],
        outputs=[ct.TensorType(name="spec_out"), ct.TensorType(name="wave_out")], convert_to="mlprogram",
        compute_precision=prec, compute_units=cu, minimum_deployment_target=ct.target.macOS13)
    print("convert", time.time()-t)
    inp = {"mix": mix.numpy(), "spec": spec.numpy()}
    ml.predict(inp)
    ts=[]
    for k in range(3):
        t=time.time(); o=ml.predict(inp); ts.append(time.time()-t)
    sdr=[10*np.log10((r.numpy()**2).sum()/((o[n]-r.numpy())**2).sum()) for n,r in zip(["spec_out","wave_out"],ref)]
    print(prec, cu, ["%.2f"%x for x in ts], "SDR vs torch", sdr)
    if prec == ct.precision.FLOAT32: ml.save("../../models/htdemucs_fp32.mlpackage")
