# Splicy

Splicy is the video editor of [SexOS](https://github.com/xirtus/sexos): a full non-linear
editor (timeline, colour grading, keying, time remapping, stabilizing, 84 transitions, 111
video effects, 55 audio effects) with transcript editing, drawn by SexOS's `sex-egui`.

## Layout

- `src/` – Splicy Studio, the program: opens clips and their transcripts, runs the editor in a
  SexOS window, and on a host paints a screenshot (`splicy-studio --demo shot.png 1280 800`).
- `core/` – the editing core: engine, renderer, codecs, export and interface crates. It began as
  FilmCraft's code (MIT OR Apache-2.0; see `core/LICENSE-MIT`, `core/LICENSE-APACHE`,
  `core/NOTICE`) and carries only Splicy's name.
- `shims/` – stand-ins for desktop-only pieces (wgpu window, GPU compositor, self-updater) that
  SexOS does not use.

## Building

Splicy builds inside the SexOS tree, where it lives as the submodule `apps/splicy-studio`
and uses SexOS crates by relative path (`../../crates/sex-egui`, `../../crates/sex-std`):

```sh
git clone --recurse-submodules https://github.com/xirtus/sexos
cd sexos
bash scripts/build_splicy_studio.sh            # the SexOS program
cd apps/splicy-studio && cargo build --profile shot   # host screenshot build
```

SexOS's CI boots the program in QEMU (`scripts/program_splicy_studio_gate.py`).

## Licence

Splicy's own code is GPL-3.0-only (`LICENSE`). The code in `core/` stays under its
MIT OR Apache-2.0 licence, as its licence files say. Remove Restarts
(`core/crates/edit/src/transcript/restarts.rs`) is adapted from Scarlet
(github.com/Forward-Future/scarlet), used under the MIT licence in `core/LICENSE-SCARLET-MIT`.

Earlier history: the commits under `apps/splicy-studio` in xirtus/sexos up to this import.
