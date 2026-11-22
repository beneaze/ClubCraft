//! The separation models ship as their own download (a "model pack"), not inside the app
//! package: they change rarely and are about 1.5 GB, while the app changes often. The app
//! downloads the pack for its backend once into [`paths::user_models_dir`], where it stays across
//! updates. `crates/app/model-packs.json` (written by `tools/package-models.sh`) pins each pack's
//! URL, size and SHA-256, so the app only installs the exact files it was released with.

use anyhow::{bail, Context, Result};
use clubcraft_core::paths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

const MANIFEST: &str = include_str!("../model-packs.json");
/// Shown where separation can't start because the models were never downloaded.
pub const MISSING: &str = "Stem models aren't installed: Settings › Analysis & stems › Download models.";
/// Written into an installed pack folder last, so a folder without it is incomplete.
const INSTALLED: &str = "pack.json";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Pack {
    pub id: String,
    /// `coreml` (macOS) or `onnx` (Windows, Linux).
    pub backend: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    packs: Vec<Pack>,
}

/// The pack this build downloads. `None` if the embedded manifest has none for this platform or
/// it was never filled in (a source build), so the UI points to the manual export instead.
pub fn platform_pack() -> Option<Pack> {
    let backend = if cfg!(target_os = "macos") { "coreml" } else { "onnx" };
    let manifest: Manifest = match serde_json::from_str(MANIFEST) {
        Ok(m) => m,
        Err(e) => {
            log::error!("model-packs.json: {e}");
            return None;
        }
    };
    manifest.packs.into_iter().find(|p| p.backend == backend && p.size > 0 && p.sha256.len() == 64)
}

/// The pack installed in `dir` by this module, if any.
pub fn installed_pack(dir: &Path) -> Option<Pack> {
    let text = std::fs::read_to_string(dir.join(INSTALLED)).ok()?;
    serde_json::from_str(&text).ok()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Running,
    Done,
    Cancelled,
    Failed(String),
}

/// Shared between the UI and the worker thread.
#[derive(Debug)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    cancel: AtomicBool,
    phase: Mutex<Phase>,
}

impl Default for Progress {
    fn default() -> Self {
        Progress { done: AtomicU64::new(0), total: AtomicU64::new(0), cancel: AtomicBool::new(false), phase: Mutex::new(Phase::Idle) }
    }
}

impl Progress {
    pub fn phase(&self) -> Phase {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_phase(&self, p: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 { 0.0 } else { (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32 }
    }
}

/// Runs one download or file install at a time on a background thread.
#[derive(Default)]
pub struct Installer {
    progress: Arc<Progress>,
}

/// Where a pack comes from.
pub enum Source {
    Download,
    File(PathBuf),
}

impl Installer {
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    pub fn running(&self) -> bool {
        self.progress.phase() == Phase::Running
    }

    pub fn start(&mut self, pack: Pack, source: Source) {
        if self.running() {
            return;
        }
        let progress = Arc::new(Progress::default());
        progress.total.store(pack.size, Ordering::Relaxed);
        progress.set_phase(Phase::Running);
        self.progress = progress.clone();
        let worker = progress.clone();
        let spawned = std::thread::Builder::new().name("model-pack".into()).spawn(move || {
            let dest = paths::user_models_dir();
            let result = match source {
                Source::Download => download(&pack, &dest, &worker),
                Source::File(path) => std::fs::File::open(&path)
                    .with_context(|| format!("open {}", path.display()))
                    .and_then(|f| install(f, &pack, &dest, &worker)),
            };
            match result {
                Ok(()) => {
                    log::info!("installed model pack {} into {}", pack.id, dest.display());
                    worker.set_phase(Phase::Done);
                }
                Err(_) if worker.cancel.load(Ordering::Relaxed) => worker.set_phase(Phase::Cancelled),
                Err(e) => {
                    log::error!("model pack {}: {e:#}", pack.id);
                    worker.set_phase(Phase::Failed(format!("{e:#}")));
                }
            }
        });
        if let Err(e) = spawned {
            progress.set_phase(Phase::Failed(format!("could not start the download: {e}")));
        }
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

fn download(pack: &Pack, dest: &Path, progress: &Progress) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        // Per read, not for the whole 1.5 GB.
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent(concat!("ClubCraft/", env!("CARGO_PKG_VERSION")))
        .build();
    let response = agent.get(&pack.url).call().with_context(|| format!("download {}", pack.url))?;
    install(response.into_reader(), pack, dest, progress)
}

/// Hashes and counts everything read through it, and stops when the user cancels.
struct Checked<'a, R> {
    inner: R,
    hash: Sha256,
    count: u64,
    progress: &'a Progress,
}

impl<R: Read> Read for Checked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.progress.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.count += n as u64;
        self.progress.done.store(self.count, Ordering::Relaxed);
        Ok(n)
    }
}

/// Unpack the pack tar from `reader` into `dest`. The files are unpacked while they stream in
/// (no second 1.5 GB copy on disk) into a staging folder next to `dest`, and only moved into place
/// after the size and checksum of the whole stream match the manifest. On any error `dest` is
/// left as it was.
pub fn install(reader: impl Read, pack: &Pack, dest: &Path, progress: &Progress) -> Result<()> {
    let parent = dest.parent().context("models folder has no parent")?;
    let name = dest.file_name().context("models folder has no name")?.to_string_lossy().into_owned();
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let staging = parent.join(format!("{name}.partial"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    let result = unpack_checked(reader, pack, &staging, progress).and_then(|()| replace_dir(&staging, dest, parent, &name));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn unpack_checked(reader: impl Read, pack: &Pack, staging: &Path, progress: &Progress) -> Result<()> {
    let mut checked = Checked { inner: reader, hash: Sha256::new(), count: 0, progress };
    {
        let mut archive = tar::Archive::new(&mut checked);
        for entry in archive.entries().context("read model pack")? {
            let mut entry = entry.context("read model pack")?;
            let path = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
            // `unpack_in` refuses paths that leave the folder (`..`, absolute paths).
            if !entry.unpack_in(staging).with_context(|| format!("unpack {path}"))? {
                bail!("model pack contains an unsafe path: {path}");
            }
        }
    }
    // The tar reader stops at the end marker; the checksum covers the padding after it too.
    std::io::copy(&mut checked, &mut std::io::sink()).context("read model pack")?;
    if checked.count != pack.size {
        bail!("model pack is {} bytes, expected {} (incomplete download?)", checked.count, pack.size);
    }
    let sum: String = checked.hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !sum.eq_ignore_ascii_case(&pack.sha256) {
        bail!("model pack checksum does not match (corrupt download or a different pack version)");
    }
    if !paths::has_models(staging) {
        bail!("model pack contains no separation models");
    }
    let marker = serde_json::to_string_pretty(pack).context("write pack.json")?;
    std::fs::write(staging.join(INSTALLED), marker).context("write pack.json")?;
    Ok(())
}

/// Swap `staging` in for `dest`, keeping the old folder until the new one is in place.
fn replace_dir(staging: &Path, dest: &Path, parent: &Path, name: &str) -> Result<()> {
    let old = parent.join(format!("{name}.old"));
    let _ = std::fs::remove_dir_all(&old);
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &old).with_context(|| {
            format!("replace {} (are the models in use? restart ClubCraft and try again)", dest.display())
        })?;
    }
    if let Err(e) = std::fs::rename(staging, dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(e).with_context(|| format!("move the models into {}", dest.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut h = tar::Header::new_ustar();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn pack_for(bytes: &[u8]) -> Pack {
        let sum: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        Pack { id: "test-v1-onnx".into(), backend: "onnx".into(), url: String::new(), size: bytes.len() as u64, sha256: sum }
    }

    const GOOD: &[(&str, &[u8])] = &[("htdemucs.json", b"{}"), ("htdemucs.onnx", b"weights"), ("htdemucs_ft_0.mlmodelc/model.mil", b"mil")];

    #[test]
    fn installs_a_verified_pack_and_replaces_the_old_one() {
        let root = tmp("ok");
        let dest = root.join("Models");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.onnx"), b"old").unwrap();
        let bytes = tar_of(GOOD);
        let pack = pack_for(&bytes);
        let progress = Progress::default();
        install(&bytes[..], &pack, &dest, &progress).unwrap();
        assert_eq!(std::fs::read(dest.join("htdemucs.onnx")).unwrap(), b"weights");
        assert!(dest.join("htdemucs_ft_0.mlmodelc/model.mil").exists());
        assert!(!dest.join("stale.onnx").exists());
        assert!(paths::has_models(&dest));
        assert_eq!(installed_pack(&dest), Some(pack));
        assert_eq!(progress.done.load(Ordering::Relaxed), bytes.len() as u64);
        assert!(!root.join("Models.partial").exists() && !root.join("Models.old").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_wrong_checksum_or_size_leaves_the_installed_models_alone() {
        let root = tmp("bad");
        let dest = root.join("Models");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("htdemucs.onnx"), b"installed").unwrap();
        let bytes = tar_of(GOOD);
        let mut wrong_sum = pack_for(&bytes);
        wrong_sum.sha256 = "0".repeat(64);
        let err = install(&bytes[..], &wrong_sum, &dest, &Progress::default()).unwrap_err();
        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        let truncated = &bytes[..bytes.len() - 512];
        let err = install(truncated, &pack_for(&bytes), &dest, &Progress::default()).unwrap_err();
        assert!(format!("{err:#}").contains("expected"), "{err:#}");
        assert_eq!(std::fs::read(dest.join("htdemucs.onnx")).unwrap(), b"installed");
        assert!(!root.join("Models.partial").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_paths_outside_the_models_folder_and_packs_without_weights() {
        let root = tmp("unsafe");
        let dest = root.join("Models");
        // tar::Builder refuses `..` itself, so write the raw header name.
        let mut h = tar::Header::new_ustar();
        h.as_ustar_mut().unwrap().name[..12].copy_from_slice(b"../evil.onnx");
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        let mut b = tar::Builder::new(Vec::new());
        b.append(&h, &b"x"[..]).unwrap();
        let bytes = b.into_inner().unwrap();
        let err = install(&bytes[..], &pack_for(&bytes), &dest, &Progress::default()).unwrap_err();
        assert!(format!("{err:#}").contains("unsafe"), "{err:#}");
        assert!(!root.join("evil.onnx").exists() && !dest.exists());

        let bytes = tar_of(&[("htdemucs.json", b"{}")]);
        let err = install(&bytes[..], &pack_for(&bytes), &dest, &Progress::default()).unwrap_err();
        assert!(format!("{err:#}").contains("no separation models"), "{err:#}");
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelling_stops_without_installing() {
        let root = tmp("cancel");
        let dest = root.join("Models");
        let bytes = tar_of(GOOD);
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(install(&bytes[..], &pack_for(&bytes), &dest, &progress).is_err());
        assert!(!dest.exists() && !root.join("Models.partial").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The packs built by `tools/package-models.sh` match the embedded manifest and unpack with
    /// this reader: `cargo test -p clubcraft models::tests::real_pack -- --ignored`
    /// (`CLUBCRAFT_TEST_PACK=onnx` checks the ONNX pack on macOS).
    #[test]
    #[ignore = "needs dist/models from tools/package-models.sh (1.5 GB)"]
    fn real_pack_from_dist_installs_and_verifies() {
        let pack = match std::env::var("CLUBCRAFT_TEST_PACK") {
            Ok(backend) => serde_json::from_str::<Manifest>(MANIFEST).unwrap().packs.into_iter().find(|p| p.backend == backend).unwrap(),
            Err(_) => platform_pack().expect("manifest has a pack for this platform"),
        };
        let name = pack.url.rsplit('/').next().unwrap();
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist/models").join(name);
        let root = tmp("real");
        let dest = root.join("Models");
        install(std::fs::File::open(&file).unwrap(), &pack, &dest, &Progress::default()).unwrap();
        assert!(paths::has_models(&dest));
        for m in ["htdemucs.json", "htdemucs_ft.json", "melroformer.json", "allin1.onnx", "cocola.onnx", "cocola_w.bin"] {
            assert!(dest.join(m).exists(), "{m}");
        }
        // The installed folder works for separation, not just has the right names.
        let sep = clubcraft_stems::Separator::with_models_dir(clubcraft_stems::Quality::Fast, &dest).unwrap();
        let frames = 10 * 44100;
        let tone: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let audio = clubcraft_audio::AudioBuffer::new(44100, tone.clone(), tone);
        let stems = sep.separate(&audio, &mut |_| true).unwrap();
        assert_eq!(stems.frames(), frames);
        eprintln!("installed {name} and separated with {} on {}", sep.model_name(), sep.backend_name());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_embedded_manifest_parses_and_names_both_backends() {
        let m: Manifest = serde_json::from_str(MANIFEST).unwrap();
        for backend in ["coreml", "onnx"] {
            let p = m.packs.iter().find(|p| p.backend == backend).unwrap();
            assert!(p.url.starts_with("https://github.com/") && p.url.ends_with(".tar"), "{}", p.url);
        }
    }
}
