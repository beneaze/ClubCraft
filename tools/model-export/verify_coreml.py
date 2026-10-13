import sys, time, numpy as np, onnxruntime as ort
name=sys.argv[1]
mix = np.load(f"../../models/{name}_ref_mix.npy"); spec = np.load(f"../../models/{name}_ref_spec.npy")
s0 = ort.InferenceSession(f"../../models/{name}.onnx", providers=["CPUExecutionProvider"])
r = s0.run(None, {"mix": mix, "spec": spec})
for units in ["CPUAndGPU", "ALL"]:
    opts = {"ModelFormat": "MLProgram", "MLComputeUnits": units, "RequireStaticInputShapes": "1"}
    s = ort.InferenceSession(f"../../models/{name}.onnx", providers=[("CoreMLExecutionProvider", opts), "CPUExecutionProvider"])
    for k in range(3):
        t=time.time(); o = s.run(None, {"mix": mix, "spec": spec}); dt=time.time()-t
    errs=[np.nanmax(np.abs(a-b)) for a,b in zip(o,r)]
    print(units, f"{dt:.2f}s", errs, [10*np.log10((b**2).sum()/((a-b)**2).sum()) for a,b in zip(o,r)])
