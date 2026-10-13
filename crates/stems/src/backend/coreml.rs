//! CoreML backend: loads the compiled ML Program (`<name>.mlmodelc`, fp32) and runs it on
//! CPU+GPU. Inputs wrap our buffers without copying; outputs are read through
//! `getBytesWithHandler`, honouring the (possibly padded) strides CoreML reports.

use super::{Backend, SourceOut};
use crate::model::Manifest;
use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration,
    MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};
use std::ffi::c_void;
use std::ptr::NonNull;

struct Model(Retained<MLModel>);
// SAFETY: MLModel prediction is documented thread-safe; we never mutate the model after loading.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

pub struct CoreMlBackend {
    models: Vec<Model>,
    l: usize,
    f: usize,
    t: usize,
    s: usize,
}

fn ns_err(e: Retained<NSError>) -> anyhow::Error {
    anyhow!("{}", e.localizedDescription())
}

fn ns_shape(v: &[usize]) -> Retained<NSArray<NSNumber>> {
    let nums: Vec<Retained<NSNumber>> = v.iter().map(|&x| NSNumber::new_isize(x as isize)).collect();
    NSArray::from_retained_slice(&nums)
}

fn contiguous_strides(shape: &[usize]) -> Vec<usize> {
    let mut st = vec![1usize; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        st[i] = st[i + 1] * shape[i + 1];
    }
    st
}

/// Wrap `data` (contiguous, f32) as an MLMultiArray without copying. The array must not outlive `data`.
unsafe fn wrap(data: &[f32], shape: &[usize]) -> Result<Retained<MLMultiArray>> {
    let ptr = NonNull::new(data.as_ptr() as *mut c_void).ok_or_else(|| anyhow!("null"))?;
    MLMultiArray::initWithDataPointer_shape_dataType_strides_deallocator_error(
        MLMultiArray::alloc(),
        ptr,
        &ns_shape(shape),
        MLMultiArrayDataType::Float32,
        &ns_shape(&contiguous_strides(shape)),
        None,
    )
    .map_err(ns_err)
}

fn numbers(a: &NSArray<NSNumber>) -> Vec<usize> {
    a.iter().map(|n| n.integerValue() as usize).collect()
}

/// Copy selected leading-index slices of an MLMultiArray of shape `[1, S, A, B, ..]` into contiguous
/// buffers, respecting strides and converting fp16 if needed.
unsafe fn read_sources(arr: &MLMultiArray, inner: &[usize], sources: &[usize], dst: &mut [&mut Vec<f32>]) -> Result<()> {
    let shape = numbers(&arr.shape());
    let strides = numbers(&arr.strides());
    if shape.len() != 2 + inner.len() || shape[0] != 1 || &shape[2..] != inner {
        bail!("unexpected CoreML output shape {shape:?}");
    }
    let dtype = arr.dataType();
    let elem = if dtype == MLMultiArrayDataType::Float32 {
        4
    } else if dtype == MLMultiArrayDataType::Float16 {
        2
    } else {
        bail!("unsupported CoreML output type {:?}", dtype);
    };
    let n: usize = inner.iter().product();
    for d in dst.iter_mut() {
        d.clear();
        d.resize(n, 0.0);
    }
    let dst_ptrs: Vec<*mut f32> = dst.iter_mut().map(|d| d.as_mut_ptr()).collect();
    let inner_strides = strides[2..].to_vec();
    let s_stride = strides[1];
    let inner = inner.to_vec();
    let mut err = None;
    let err_ptr: *mut Option<String> = &mut err;
    let block = RcBlock::new(move |bytes: NonNull<c_void>, size: isize| {
        let base = bytes.as_ptr() as *const u8;
        let size = size as usize;
        // Highest element offset touched for any source.
        let max_off: usize =
            sources.iter().map(|&s| s * s_stride).max().unwrap_or(0) + inner.iter().zip(&inner_strides).map(|(d, s)| (d - 1) * s).sum::<usize>();
        if (max_off + 1) * elem > size {
            unsafe { *err_ptr = Some(format!("CoreML buffer too small ({size} bytes)")) };
            return;
        }
        for (k, &s) in sources.iter().enumerate() {
            let out = dst_ptrs[k];
            let src0 = s * s_stride;
            // Iterate over all but the last dimension; the last is usually contiguous.
            let last = *inner.last().unwrap();
            let last_stride = *inner_strides.last().unwrap();
            let outer: usize = inner[..inner.len() - 1].iter().product();
            let mut idx = vec![0usize; inner.len() - 1];
            for o in 0..outer {
                let mut off = src0;
                for (i, &v) in idx.iter().enumerate() {
                    off += v * inner_strides[i];
                }
                unsafe {
                    let dstrow = out.add(o * last);
                    if elem == 4 {
                        let p = base.add(off * 4) as *const f32;
                        if last_stride == 1 {
                            std::ptr::copy_nonoverlapping(p, dstrow, last);
                        } else {
                            for j in 0..last {
                                *dstrow.add(j) = *p.add(j * last_stride);
                            }
                        }
                    } else {
                        let p = base.add(off * 2) as *const u16;
                        for j in 0..last {
                            *dstrow.add(j) = f16_to_f32(*p.add(j * last_stride));
                        }
                    }
                }
                // increment multi-index
                for i in (0..idx.len()).rev() {
                    idx[i] += 1;
                    if idx[i] < inner[i] {
                        break;
                    }
                    idx[i] = 0;
                }
            }
        }
    });
    arr.getBytesWithHandler(&block);
    drop(block);
    match err {
        Some(e) => bail!(e),
        None => Ok(()),
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign
        } else {
            // subnormal
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (man << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

fn load_mlmodel(path: &std::path::Path) -> Result<Retained<MLModel>> {
    if !path.exists() {
        bail!("missing {}", path.display());
    }
    autoreleasepool(|_| unsafe {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
        let cfg = MLModelConfiguration::new();
        cfg.setComputeUnits(MLComputeUnits::CPUAndGPU);
        MLModel::modelWithContentsOfURL_configuration_error(&url, &cfg).map_err(ns_err)
    })
}

/// A CoreML model with one f32 input and one f32 output of fixed shapes.
pub struct CoreMlSingle {
    model: Model,
    input: (String, Vec<usize>),
    output: (String, Vec<usize>),
}

impl CoreMlSingle {
    pub fn load(path: &std::path::Path, input: (&str, &[usize]), output: (&str, &[usize])) -> Result<Self> {
        Ok(CoreMlSingle {
            model: Model(load_mlmodel(path)?),
            input: (input.0.to_string(), input.1.to_vec()),
            output: (output.0.to_string(), output.1.to_vec()),
        })
    }
}

impl super::SingleBackend for CoreMlSingle {
    fn name(&self) -> &'static str {
        "coreml-gpu"
    }

    fn run(&self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let n: usize = self.input.1.iter().product();
        if input.len() != n {
            bail!("bad input size {} (want {n})", input.len());
        }
        autoreleasepool(|_| unsafe {
            let arr = wrap(input, &self.input.1)?;
            let val: Retained<AnyObject> =
                Retained::into_super(Retained::into_super(MLFeatureValue::featureValueWithMultiArray(&arr)));
            let key = NSString::from_str(&self.input.0);
            let dict: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(&[&*key], &[&*val]);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dict)
                .map_err(ns_err)?;
            let result = self.model.0.predictionFromFeatures_error(ProtocolObject::from_ref(&*provider)).map_err(ns_err)?;
            let o = result
                .featureValueForName(&NSString::from_str(&self.output.0))
                .and_then(|v| v.multiArrayValue())
                .ok_or_else(|| anyhow!("CoreML output {} missing", self.output.0))?;
            // Treat [d0, d1, ...] as [1, d0, rest] so the strided reader can be reused.
            let shape = &self.output.1;
            let mut d = [out];
            read_sources_shaped(&o, shape, &mut d)?;
            drop(arr);
            Ok(())
        })
    }
}

/// Read a whole MLMultiArray of exactly `shape` into a contiguous buffer.
unsafe fn read_sources_shaped(arr: &MLMultiArray, shape: &[usize], dst: &mut [&mut Vec<f32>; 1]) -> Result<()> {
    let got = numbers(&arr.shape());
    if got != shape {
        bail!("unexpected CoreML output shape {got:?} (want {shape:?})");
    }
    // [1, d1, d2..] with source index 0 along d1? Use a virtual leading [1, 1] view instead.
    let strides = numbers(&arr.strides());
    let dtype = arr.dataType();
    let n: usize = shape.iter().product();
    let out = &mut *dst[0];
    out.clear();
    out.resize(n, 0.0);
    let ptr = out.as_mut_ptr();
    let shape = shape.to_vec();
    let mut err: Option<String> = None;
    let err_ptr: *mut Option<String> = &mut err;
    let elem = if dtype == MLMultiArrayDataType::Float32 { 4 } else if dtype == MLMultiArrayDataType::Float16 { 2 } else { 0 };
    if elem == 0 {
        bail!("unsupported CoreML output type {:?}", dtype);
    }
    let block = RcBlock::new(move |bytes: NonNull<c_void>, size: isize| {
        let base = bytes.as_ptr() as *const u8;
        let max_off: usize = shape.iter().zip(&strides).map(|(d, s)| (d - 1) * s).sum();
        if (max_off + 1) * elem > size as usize {
            unsafe { *err_ptr = Some(format!("CoreML buffer too small ({size} bytes)")) };
            return;
        }
        let last = *shape.last().unwrap();
        let last_stride = *strides.last().unwrap();
        let outer: usize = shape[..shape.len() - 1].iter().product();
        let mut idx = vec![0usize; shape.len() - 1];
        for o in 0..outer {
            let off: usize = idx.iter().zip(&strides).map(|(v, s)| v * s).sum();
            unsafe {
                let row = ptr.add(o * last);
                for j in 0..last {
                    *row.add(j) = if elem == 4 {
                        *(base.add(off * 4) as *const f32).add(j * last_stride)
                    } else {
                        f16_to_f32(*(base.add(off * 2) as *const u16).add(j * last_stride))
                    };
                }
            }
            for i in (0..idx.len()).rev() {
                idx[i] += 1;
                if idx[i] < shape[i] {
                    break;
                }
                idx[i] = 0;
            }
        }
    });
    arr.getBytesWithHandler(&block);
    drop(block);
    match err {
        Some(e) => bail!(e),
        None => Ok(()),
    }
}

impl CoreMlBackend {
    pub fn load(man: &Manifest) -> Result<Self> {
        let mut models = Vec::new();
        for name in &man.models {
            models.push(Model(load_mlmodel(&man.dir.join(format!("{name}.mlmodelc")))?));
        }
        Ok(CoreMlBackend {
            models,
            l: man.segment_samples,
            f: man.freqs(),
            t: man.spec_frames,
            s: man.sources.len(),
        })
    }
}

impl Backend for CoreMlBackend {
    fn name(&self) -> &'static str {
        "coreml-gpu"
    }

    fn run(&self, m: usize, mix: &[f32], spec: &[f32], sources: &[usize], out: &mut [SourceOut]) -> Result<()> {
        let (l, f, t) = (self.l, self.f, self.t);
        if mix.len() != 2 * l || spec.len() != 4 * f * t || sources.iter().any(|&s| s >= self.s) {
            bail!("bad input sizes");
        }
        autoreleasepool(|_| unsafe {
            let mix_a = wrap(mix, &[1, 2, l])?;
            let spec_a = wrap(spec, &[1, 4, f, t])?;
            let vals: [Retained<AnyObject>; 2] = [
                Retained::into_super(Retained::into_super(MLFeatureValue::featureValueWithMultiArray(&mix_a))),
                Retained::into_super(Retained::into_super(MLFeatureValue::featureValueWithMultiArray(&spec_a))),
            ];
            let keys = [NSString::from_str("mix"), NSString::from_str("spec")];
            let dict: Retained<NSDictionary<NSString, AnyObject>> =
                NSDictionary::from_slices(&[&*keys[0], &*keys[1]], &[&*vals[0], &*vals[1]]);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dict)
                .map_err(ns_err)?;
            let input: &ProtocolObject<dyn MLFeatureProvider> = ProtocolObject::from_ref(&*provider);
            let result = self.models[m].0.predictionFromFeatures_error(input).map_err(ns_err)?;
            let get = |name: &str| -> Result<Retained<MLMultiArray>> {
                result
                    .featureValueForName(&NSString::from_str(name))
                    .and_then(|v| v.multiArrayValue())
                    .ok_or_else(|| anyhow!("CoreML output {name} missing"))
            };
            let so = get("spec_out")?;
            let wo = get("wave_out")?;
            {
                let mut d: Vec<&mut Vec<f32>> = out.iter_mut().map(|o| &mut o.spec).collect();
                read_sources(&so, &[4, f, t], sources, &mut d)?;
            }
            {
                let mut d: Vec<&mut Vec<f32>> = out.iter_mut().map(|o| &mut o.wave).collect();
                read_sources(&wo, &[2, l], sources, &mut d)?;
            }
            // Keep the wrapped inputs alive until the prediction is fully done.
            drop((mix_a, spec_a));
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn half() {
        assert_eq!(super::f16_to_f32(0x3c00), 1.0);
        assert_eq!(super::f16_to_f32(0xc000), -2.0);
        assert_eq!(super::f16_to_f32(0x0001), 5.960_464_5e-8);
    }
}
