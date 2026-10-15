//! Minimal `.npy` reader/writer for little-endian f32 C-order arrays (reference tensors, debugging).

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

/// A dense f32 array with its shape.
#[derive(Debug, Clone)]
pub struct NpyArray {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

pub fn read_npy(path: &Path) -> Result<NpyArray> {
    let b = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    parse_npy(&b)
}

pub fn parse_npy(b: &[u8]) -> Result<NpyArray> {
    if b.len() < 10 || &b[..6] != b"\x93NUMPY" {
        bail!("not an npy file");
    }
    let major = b[6];
    let (hlen, hstart) = if major == 1 {
        (u16::from_le_bytes([b[8], b[9]]) as usize, 10)
    } else {
        (u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize, 12)
    };
    let header = std::str::from_utf8(b.get(hstart..hstart + hlen).ok_or_else(|| anyhow!("truncated header"))?)?;
    if !header.contains("'descr': '<f4'") {
        bail!("only little-endian f32 supported: {header}");
    }
    if header.contains("'fortran_order': True") {
        bail!("fortran order not supported");
    }
    let s = header.find("'shape': (").ok_or_else(|| anyhow!("no shape"))? + 10;
    let e = s + header[s..].find(')').ok_or_else(|| anyhow!("bad shape"))?;
    let shape: Vec<usize> =
        header[s..e].split(',').map(str::trim).filter(|t| !t.is_empty()).map(|t| t.parse()).collect::<Result<_, _>>()?;
    let n: usize = shape.iter().product();
    let body = &b[hstart + hlen..];
    if body.len() < n * 4 {
        bail!("truncated data");
    }
    let data = body[..n * 4].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    Ok(NpyArray { shape, data })
}

pub fn write_npy(path: &Path, shape: &[usize], data: &[f32]) -> Result<()> {
    let dims: Vec<String> = shape.iter().map(|d| d.to_string()).collect();
    let shape_s = if dims.len() == 1 { format!("({},)", dims[0]) } else { format!("({})", dims.join(", ")) };
    let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': {shape_s}, }}");
    while (10 + header.len() + 1) % 64 != 0 {
        header.push(' ');
    }
    header.push('\n');
    let mut out = Vec::with_capacity(10 + header.len() + data.len() * 4);
    out.extend_from_slice(b"\x93NUMPY\x01\x00");
    out.extend_from_slice(&(header.len() as u16).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, out)?;
    Ok(())
}
