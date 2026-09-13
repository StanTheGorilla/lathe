<img src="assets/banner.svg" alt="Lathe" width="206" height="68">

<p>
  <a href="#how-it-works"><img alt="Windows 10/11" src="https://img.shields.io/badge/Windows-10%20%2F%2011-1c1c1a?style=flat-square&labelColor=35342f"></a>
  <a href="#building"><img alt="Rust" src="https://img.shields.io/badge/Rust-stable%20MSVC-d97757?style=flat-square&labelColor=35342f"></a>
  <a href="#hardware"><img alt="Vulkan" src="https://img.shields.io/badge/GPU-Vulkan-6a9bcc?style=flat-square&labelColor=35342f"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/licence-MIT-788c5d?style=flat-square&labelColor=35342f"></a>
</p>

## Why this exists

Superwhisper and the rest charge a subscription to run a model on hardware you already
own. I did not want to pay rent on my own GPU, so this is the local alternative: same
job, no account, no fee.

## How it works

Recording starts the moment you press the key. On release, a local voice-activity gate
trims the silence, a speech model transcribes on the GPU, a second model punctuates and
capitalises, and the text is pasted into the focused window.

There is no window until you open the settings, and no models in memory until the first
dictation. Both are freed again when you are done.

## What is in the box

| | |
|---|---|
| **Push-to-talk or tap-to-toggle** | Hold for as long as you speak, or tap once to start and again to stop. One threshold decides which you meant. |
| **Presets** | Bundles of tone, structure, context and language. Switch from the tray, or bind a preset to its own hotkey. |
| **Vocabulary** | Word lists that repair the proper nouns recognisers always get wrong, matched phonetically rather than by a blunt find-and-replace. |
| **Polish, and 20+ other languages** | Non-English dictation is recognised *and* cleaned, via a second multilingual model. |
| **History** | The last 200 dictations, searchable, re-pastable, stored in a local SQLite file. |
| **Audio ducking** | Whatever is playing is silenced while you talk, and restored exactly as it was. |
| **Cues, not popups** | Distinct sounds for start, stop and failure, synthesised at runtime. Nothing steals focus. |

## Measured

On an AMD RX 6600 XT (8 GB), Ryzen 7 5700G, from the project's own benchmark and
accuracy harness:

| | |
|---|---|
| Recognition | **~0.3 s** for 8 s of speech — roughly 24x realtime |
| Cleanup (English) | ~0.2 s |
| Word error rate, ordinary speech | **~0%** on the plain-prose sentences in `assets/accuracy-en.txt` |
| Word error rate, overall | 18.8% English, 13.6% Polish — concentrated almost entirely in proper nouns and jargon |
| Idle footprint | No webview, no models resident |

Reproduce it yourself:

```powershell
# Read 17 sentences aloud; it scores what the recogniser heard against what you read.
.\target\release\lathe-spike.exe --models models --speech-model cohere-transcribe-q8_0.gguf accuracy
```

Recordings are kept, so scoring a different model afterwards reuses the same audio
rather than asking you to read everything again.

## Models

Speech is Cohere Transcribe (Q8_0), cleanup is S1-mini for English and Gemma 4 E2B for
everything else, all fetched on first run. Superwhisper publishes S1-mini only at F16
and Q4_K_M, and Q4_K_M was measured dropping whole clauses, so Lathe ships two builds
of its own at
[stanthegorilla/S1-mini-Q8_0-Q6_K-GGUF](https://huggingface.co/stanthegorilla/S1-mini-Q8_0-Q6_K-GGUF):

| Build | Size | Output identical to F16 | Decode |
|---|---|---|---|
| Q8_0, the default | 805 MB | 94% / 95% (synthetic / real dictations) | 246 tok/s |
| Q6_K mixed, calibrated on real dictations | 636 MB | 86% / 92%, the rest punctuation | 272 tok/s |
| F16, upstream | 1,509 MB | reference | 150 tok/s |

Neither dropped content on 202 test inputs. Fourteen builds were measured to get here;
the harness is `lathe-spike cleanup-eval` and `scripts/quant/`.

## Hardware

Today the GPU path is **Vulkan** on Windows and Linux, which covers AMD, Intel and
NVIDIA through one backend, and **Metal** on macOS. Vulkan was chosen so a single build
runs on any reasonably modern GPU without per-vendor packaging.

Broader backend coverage — CUDA and DirectML in particular, and a build that picks the
best available at runtime — is the largest open piece of work.

## Building

Windows is the platform this is written and used on. macOS and Linux builds exist and compile
in CI, but nobody has dictated with them yet; see
[Other platforms](#other-platforms) below.

The Windows build has real prerequisites:

- Rust (stable, MSVC toolchain)
- Visual Studio Build Tools with the Windows SDK
- The **Vulkan SDK** (`VULKAN_SDK` must be set)
- CMake and **Ninja** — the Visual Studio CMake generator does not survive the nested
  shader-compiler builds that whisper.cpp and llama.cpp use
- Node.js, for the settings UI

```powershell
git clone https://github.com/StanTheGorilla/lathe.git
cd lathe
.\scripts\fetch-models.ps1        # downloads the GGUF weights
.\scripts\build.ps1               # release build
.\scripts\build.ps1 -Bundle       # ...and an NSIS installer
```

`scripts/build.ps1` exists because neither step is a plain `cargo build`: it imports the
MSVC environment, forces the Ninja generator, and copies the CrispASR runtime DLLs next
to the executable. The reasoning is written out at the top of the script.

### Other platforms

`scripts/build.sh` is the macOS and Linux counterpart. It needs Rust, CMake, Ninja and
Node.js; Linux additionally needs the Vulkan headers and `glslc`, plus the WebKitGTK,
GTK 3, libayatana-appindicator and ALSA development packages that any Tauri app needs.
`.github/workflows/build.yml` has the exact `apt` and `brew` lines.

```sh
./scripts/build.sh              # release build
./scripts/build.sh --bundle     # ...and a .dmg, or a .deb and an AppImage
```

What differs from Windows, all of it by necessity rather than choice:

- **macOS** needs the Accessibility permission (System Settings > Privacy & Security)
  to hear the hotkey and to type; it asks on first launch. The default binding is
  **Option+Space**. Other applications cannot be quietened while recording, because
  macOS has no per-application volume.
- **Linux** reads the keyboard through `/dev/input`, so the user must be in the `input`
  group, and pastes through a `uinput` virtual keyboard, which needs a udev rule:
  `KERNEL=="uinput", GROUP="input", MODE="0660"` in
  `/etc/udev/rules.d/70-lathe.rules`. This works under X11 and Wayland alike, but the
  hotkey is not swallowed -- the focused application sees it too -- so the default is
  **Ctrl+Alt+Space** and anything an editor already uses is a poor choice. All text
  goes through the clipboard; there is no typed path. Ducking uses `pactl`, which
  works with PulseAudio and PipeWire.
- The models download from the Models tab on every platform; `fetch-models.ps1` is
  Windows-only.

## Configuration

Everything lives in one hand-editable file:

```
%APPDATA%\Lathe\config.toml
```

The settings window writes the same file, and the running app reloads it on change. If
you would rather never open the window, you never have to.

## Repository layout

```
crates/core          audio, models, vocabulary, history, the pipeline
crates/lathe         the tray app: hotkey hook, worker thread, Tauri shell
crates/spike         measurement CLI — benchmark, accuracy harness, device probes
crates/cohere-probe  a standalone check that the speech model runs at all
ui/                  the settings window (Svelte)
scripts/             build, model download, asset generation
```

## Licence

MIT. See [LICENSE](LICENSE).

Model weights are downloaded separately and carry their own licences — Cohere
Transcribe, S1-mini, Gemma 4 and Silero VAD each have their own terms.
