import time, numpy as np, onnxruntime as ort
mix = np.load("../../models/htdemucs_ref_mix.npy"); spec = np.load("../../models/htdemucs_ref_spec.npy")
for th in [4, 8, 10]:
    so = ort.SessionOptions(); so.intra_op_num_threads = th; so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    s = ort.InferenceSession("../../models/htdemucs.onnx", so, providers=["CPUExecutionProvider"])
    s.run(None, {"mix": mix, "spec": spec})
    ts=[]
    for k in range(3):
        t=time.time(); s.run(None, {"mix": mix, "spec": spec}); ts.append(time.time()-t)
    print(th, ["%.2f"%x for x in ts])
