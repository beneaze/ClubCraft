# Clean-room policy

ClubCraft is an independent implementation. It works with files and behaviour of other DJ
software (DJ.Studio, VirtualDJ, Mixed In Key) for interoperability. Their code is never used
or read.

## Allowed sources

- **Observation:** the other application's own data files and exports on the developer's
  machine (project files, libraries, cue sheets, rendered mixes), screenshots and screen
  recordings of the application in use, and measurements of its output.
- **Public documentation:** manuals, help centres, public wikis and forums, file-format
  specifications.
- **Comparison of results:** for example, comparing ClubCraft's beat grids, keys and cues with
  those another application stored for the same track.

## Not allowed

Reading, decompiling or extracting another application's program code (binaries, bundled or
minified JavaScript, shaders, models), and copying its assets. Notes written by anyone who did
read such code must not be given to whoever implements the feature.

## History

**October 2026: DJ.Studio importer.** Parts of the DJ.Studio importer and some related defaults
had been derived from reading DJ.Studio's bundled JavaScript. They were removed in
the commit that introduced this document:

- **The importer** (`crates/app/src/interop/djstudio.rs`): the project, transition, stem and
  tempo conversion.
- **The per-transition tempo model:** the `TempoRamp` type and the `MixTrack::line_bpm` field.
- **Defaults taken from that code:** the planner's default transition length, the cue
  generator's fallback mix points, and the curve editor's drag-to-delete.
- **The benchmark's key lookup table** (`crates/analysis/examples/eval.rs`), which reads
  DJ.Studio's key numbers.

They are being re-implemented under the rules above, by an implementer who has not seen
DJ.Studio's program code, the removed code, or notes derived from it. The specification
they work from is [`interop/djstudio.md`](interop/djstudio.md); every fact in it names its
observational or public source. The removed code remains in this repository's git history.

The VirtualDJ export was built from VirtualDJ's resource and database files, read as data,
and from the public VirtualDJ wiki.
