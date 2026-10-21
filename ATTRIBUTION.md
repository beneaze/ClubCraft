# Attribution

ClubCraft is © 2026 the ClubCraft contributors, dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE). This page lists the third-party work it uses or ships, each under
its own licence.

## Models

The model weights are not in git. They are exported from the official checkpoints with the
scripts in [`tools/model-export`](tools/model-export) (see [`models/README.md`](models/README.md))
and ship in the release packages.

| Model | Used for | Authors | Licence |
|---|---|---|---|
| [Demucs v4](https://github.com/facebookresearch/demucs) (`htdemucs`, `htdemucs_ft`, `htdemucs_6s`) | Stem separation (Fast, High; drums and bass in Best) | Meta Platforms (Rouard, Massa, Défossez) | MIT |
| [Mel-Band RoFormer vocal model](https://huggingface.co/KimberleyJSN/melbandroformer) (`MelBandRoformer.ckpt`) | Vocals in Best quality | Kimberley Jensen | MIT |
| [Music-Source-Separation-Training](https://github.com/ZFTurbo/Music-Source-Separation-Training) Mel-Band RoFormer code, vendored in `tools/model-export/mel_band_roformer/` | Model definition for the export | Roman Solovyev (ZFTurbo), based on lucidrains' BS-RoFormer | MIT |
| [All-In-One](https://github.com/mir-aidj/all-in-one) (Harmonix 8-fold ensemble) | Optional section detection | Taejun Kim, Juhan Nam | MIT |
| [COCOLA](https://github.com/gladia-research-group/cocola) (`COCOLA_HP_v1`) | Mashup finder re-ranking | Ciranni et al. | MIT, see the note below |

**COCOLA training data.** The COCOLA checkpoint was trained on MoisesDB, Slakh2100 and
CocoChorales. MoisesDB is licensed for non-commercial research only. The mashup finder works
without the COCOLA files (rule-based score only), so a commercial distribution can leave them out.

The Mel-Band RoFormer architecture is by Wang, Lu and Won (ByteDance), "Mel-Band RoFormer for
Music Source Separation" (2023). The STFT, chunking, overlap-add and masking around the exported
networks are reimplemented in Rust in `crates/stems`.

## Vendored code

| Code | Where | Authors | Licence |
|---|---|---|---|
| [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch) | `crates/engine/vendor/signalsmith-stretch` | Geraint Luff / Signalsmith Audio Ltd. | MIT |
| [Signalsmith Linear](https://github.com/Signalsmith-Audio/linear) | `crates/engine/vendor/signalsmith-linear` | Signalsmith Audio | MIT |

Both are unmodified and keep their `LICENSE.txt`.

## Runtime libraries in the release packages

| Library | Platform | Licence |
|---|---|---|
| [ONNX Runtime](https://github.com/microsoft/onnxruntime) | macOS, Windows, Linux | MIT (Microsoft) |
| DirectML (`DirectML.dll`, from the `Microsoft.AI.DirectML` NuGet package) | Windows | Microsoft's licence terms for that package |

## C libraries compiled into the app

| Library | Used for | Via | Licence |
|---|---|---|---|
| [libopus](https://opus-codec.org) | Decoding `.opus` files | `opusic-sys` (built from source) and `symphonia-adapter-libopus` | BSD-3-Clause (Xiph.Org and contributors) |

## Fonts

The UI uses egui's default fonts (`epaint_default_fonts`):

| Font | Licence |
|---|---|
| Ubuntu Light | Ubuntu Font Licence 1.0 |
| Hack | MIT (Bitstream Vera and DejaVu parts under their own permissive terms) |
| Noto Emoji | SIL Open Font Licence 1.1 |
| emoji-icon-font | MIT and SIL Open Font Licence 1.1 |

## Rust crates

The Rust dependencies are listed in [`Cargo.lock`](Cargo.lock) and are used under their own
licences (almost all MIT and/or Apache-2.0). To print the full list with licences:

```sh
cargo install cargo-license && cargo license
```

## Own work

The ClubCraft logo and icon (`docs/images/icon.png`, made by `tools/make_icon.py`), the theme and
the screenshots are ClubCraft's own.

## Trademarks and interoperability

DJ.Studio, VirtualDJ and Mixed In Key are trademarks of their respective owners. ClubCraft is not
affiliated with or endorsed by any of them. It reads and writes their file formats for
interoperability only, following the [clean-room policy](docs/clean-room.md). None of their code,
models or assets are included.
