import time, numpy as np, coremltools as ct
ml = ct.models.MLModel("../../models/htdemucs_fp32.mlpackage", compute_units=ct.ComputeUnit.CPU_AND_GPU)
inp = {"mix": np.load("../../models/htdemucs_ref_mix.npy"), "spec": np.load("../../models/htdemucs_ref_spec.npy")}
ml.predict(inp); ts=[]
for k in range(3):
    t=time.time(); o=ml.predict(inp); ts.append(time.time()-t)
print("fp32 gpu", ts)
