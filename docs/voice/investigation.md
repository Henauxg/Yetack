# Voice input: investigation

How dictation could work in the desktop app, measured in November 2026 with the bench in [experiments/voice-bench](../../experiments/voice-bench/README.md).
The bench plays recordings into each engine at the pace of speech, as a microphone would, and measures what the user would see.

## What is decided

- The app records the microphone in Rust with `cpal`, and keeps the audio in memory only: never on disk, never in logs.
- A key held down records, a toggle mode serves long dictation, and a mic button sits in the composer.
- The app shows a level meter, live text while the user speaks, a "transcribing" state, and errors.
- The text goes in at the cursor and is never sent on its own. Sending on release can be an option.
- A model downloads on first use, checked by SHA-256, and loads on first use.
- Dictation is mostly in English. Up to 1 or 2 s of delay is fine.
- Later, not in the first version: voice commands, a cloud backend with the user's key, and a cleanup pass by an AI model with the project's terms.
- Tiny crates are out: a needed one is reimplemented. `sherpa-onnx` stays a candidate for later tests: its build downloads binaries without checking them.

## Method

- 24 prompts that steer an agent on this repository, with code terms (`prompts.toml`), read by one speaker in English: 190 s in total.
- Each recording is scaled to a peak of 0.7 first, as the app's level control would do.
- Three machines in one: four performance cores of an i7-14700KF (a fast laptop), four of its efficiency cores (a slower laptop), and an RTX 4070 through Vulkan.
- The bench's README defines each number. In short:
  - **WER**: word error rate of the final text. **Live WER**: the same for the last live text before the key is released.
  - **Terms**: the share of the prompts' code terms that come out recognizable.
  - **Final**: from the release of the key to the final text, median and 90th percentile.
  - **Delay**: from the end of each spoken word to its appearance on screen.
  - **Busy**: the engine's compute time over the audio's length. Above 100%, it cannot keep up.
  - **Private**: the memory the engine allocated, without the memory-mapped model file.

## The components

| Component | What it is | Made by | License |
| --- | --- | --- | --- |
| transcribe.cpp | C++ speech library: the model code of about 20 families, on ggml | The Handy team (one main author), since April 2026 | MIT |
| ggml | The compute kernels under transcribe.cpp, for CPUs and GPUs | ggml-org, the authors of llama.cpp | MIT |
| Parakeet Ultra | NVIDIA's Parakeet TDT 0.6B v3 trained further. Transcribes whole segments | Moondream, September 2026 | CC-BY-4.0 |
| Nemotron Speech Streaming EN 0.6B | English streaming model that computes each slice of audio once | NVIDIA, March 2026 | NVIDIA Open Model License |
| Qwen3-ASR 0.6B | Speech model with a language-model decoder. Takes hints | Alibaba (Qwen) | Apache-2.0 |
| Whisper large-v3-turbo | Speech model, 99 languages. Takes hints | OpenAI | MIT |
| Silero VAD v6.2 | Speech detector, 1 MB | Silero | MIT |
| parakeet-rs | Rust model code for NVIDIA's models, on ONNX Runtime or burn | One author | MIT or Apache-2.0 |
| candle, burn | Rust neural-network libraries | Hugging Face, Tracel AI | Apache-2.0, MIT or Apache-2.0 |

All the GGUF files come from the Handy team's conversions on Hugging Face, pinned by commit and SHA-256 in `models.toml`. The 8-bit files weigh 730 MB (Nemotron), 740 MB (Ultra), 850 MB (Qwen3) and 886 MB (Whisper).

## How live text is made

- **Streaming**: Nemotron computes each new chunk once and keeps a cache of the past. It is cheap, but less accurate.
- **Again at each step**: an offline model transcribes the whole utterance again every 0.5 or 1 s. It costs more as the utterance grows.
- **Cut at pauses**: an offline model cuts the utterance at pauses of 400 ms and transcribes each finished segment once, while the user goes on. The live text comes from transcribing only the current segment again. At the release of the key, only the last segment is left.
- **Hybrid**: Nemotron makes the live text, and Ultra writes the final text segment by segment between pauses.

Pauses come from the audio's level: a 20 ms frame quieter than a tenth of the loudest one so far. Silero then checks each finished segment and skips the ones without speech.

## Results

### Fast CPU, four cores

| Design | WER | Live WER | Terms | Final (p90) | Delay | Busy | Private |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Ultra cut at pauses, Silero check | 11.6% | 11.6% | 72% | 0.25 s (0.57 s) | 0.14 s | 38% | 616 MB |
| Hybrid: Nemotron live, Ultra final, Silero check | 11.6% | 18.8% | 72% | 0.26 s (0.40 s) | 0.30 s | 20% | 1211 MB |
| Ultra again every 0.5 s | 10.9% | 13.7% | 74% | 0.71 s (1.83 s) | 0.33 s | 63% | 703 MB |
| Nemotron, 160 ms chunks | 19.1% | 19.5% | 63% | 0.06 s (0.10 s) | 0.44 s | 13% | 603 MB |
| Nemotron, 1.1 s chunks | 15.1% | 20.9% | 68% | 0.10 s (0.12 s) | 0.81 s | 8% | 610 MB |
| Parakeet unified, streaming | 10.5% | 34.7% | 74% | 0.57 s (0.90 s) | 1.87 s | 29% | 585 MB |
| Qwen3 cut at pauses, hints | 9.7%¹ | 26.0% | 87% | 1.30 s (2.15 s) | 1.14 s | 90% | 815 MB |
| Whisper turbo at the end, hints | 6.7% | none | 89% | 7.80 s (7.89 s) | none | 98% | 502 MB |

¹ Without one recording where Qwen3, on a segment without speech, wrote out its whole list of hints (34.7% with it). The Silero check prevents that.

### Slow CPU, four efficiency cores

| Design | Live WER | Final (p90) | Delay | Busy |
| --- | --- | --- | --- | --- |
| Ultra cut at pauses, Silero check | 20.9% | 1.44 s (2.37 s) | 0.57 s | 92% |
| Hybrid, Silero check | 20.2% | 1.02 s (1.41 s) | 0.48 s | 55% |
| Ultra again every 0.5 s | 33.3% | 1.95 s (3.21 s) | 0.91 s | 103% |
| Nemotron, 160 ms chunks | 20.7% | 0.23 s (0.31 s) | 0.49 s | 33% |

The final WER does not depend on the CPU: it is the same as on the fast CPU.

### GPU: RTX 4070 through Vulkan

| Design | WER | Terms | Final (p90) | Delay | CPU cores |
| --- | --- | --- | --- | --- | --- |
| Whisper turbo again every 1 s, hints | 6.7% | 89% | 0.15 s (0.21 s) | 0.26 s | 0.1 |
| Qwen3 cut at pauses, hints | 8.1% | 88% | 0.11 s (0.19 s) | 0.39 s | 0.1 |
| Ultra cut at pauses | 11.2% | 72% | 0.13 s (0.23 s) | 0.09 s | 0.3 |
| Hybrid | 11.2% | 72% | 0.10 s (0.14 s) | 0.41 s | 0.1 |

### Code terms and hints

Every model misses the same rare words without hints: tokio ("Tokyo"), musl ("muzzle"), Gentoo and Arch ("gain two on arc"), anyhow ("any whole"), alsa-lib ("Alza lib"), JSON ("gison"), Dioxus, pkg-config, and cockpit_daemon ("cockpit diamond").
Of 14 such terms, the models get 2 or 3 without hints, and Qwen3 or Whisper with hints get 8 to 10.

Only Qwen3 and Whisper take hints in transcribe.cpp, not the Parakeet family. On the CPU, hints make Qwen3 about 70% slower (busy 52% to 90%). On the GPU, they cost almost nothing.
The test passed exactly the prompts' terms, which is the best case. In the app, the hints would be the project's names, a longer and less precise list.

### Where hints could come from

Hints only pay off where they cost little, so with a GPU. One list per project could serve twice: as hints for Qwen3 or Whisper, and as replacements after any model ("Tokyo" to "tokio").

- **From the project's files**: crate and package names, file names, the identifiers in the conversation. Automatic, but long and noisy.
- **From the user's AI**: a setup step copies a prompt to the clipboard, the user gives it to their AI, and it writes the list in a file the app reads.
- **From one of the project's agents**: the app already runs Claude Code in the project's folder, so the same setup step could ask an agent there directly, with the user's agreement.

### Runtimes for the same models

| Runtime | Nemotron WER | Busy, 4 fast cores | Private | Notes |
| --- | --- | --- | --- | --- |
| transcribe.cpp (ggml), 8-bit | 15.1% (1.1 s chunks) | 8% | 610 MB | Loads in 0.2 s. Runs every model |
| parakeet-rs on ONNX Runtime, fp32 | 12.3% (560 ms chunks) | 19% | 2299 MB | Microsoft's runtime DLL |
| parakeet-rs on burn CPU (Flex), fp32 | 12.3% | 47% | 2576 MB | Pure Rust. About 3 times slower per core than ONNX Runtime |
| parakeet-rs on burn wgpu (GPU), fp32 | 12.3% | 16% | 6863 MB committed | Pure Rust. 12 to 24 s of kernel compiling at the first use |

ggml's Nemotron is less accurate than ONNX Runtime's. Whether its 8-bit weights or its code cause it is not known yet: an F16 file would tell.

### Kernels: candle against ggml

The matrix products of one encoder layer of Parakeet (feed-forward and attention), in ms, on 4 fast cores. `kernels/` in the bench holds both programs.

| Weights | Speech in the chunk | ggml, Microsoft's compiler | ggml, Microsoft's compiler with AVX-VNNI | ggml, clang tuned for this CPU | candle |
| --- | --- | --- | --- | --- | --- |
| 8-bit | 0.56 s | 0.94 | 0.82 | 0.69 | 0.69 |
| 8-bit | 5 s | 8.6 | 7.3 | 6.3 | 6.2 |
| 8-bit | 30 s | 51.3 | 44.8 | 98.7² | 39.0 |
| f32 | 5 s | 12.6 | 12.5 | 6.6 | 6.6 |
| f32 | 30 s | 72.0 | 71.9 | 37.4 | 34.2 |

² An outlier: the same product takes 155.7 ms on one thread and 21.2 ms on eight.

- Built with Microsoft's compiler, ggml's kernels are 10 to 35% slower than candle's, and its f32 kernels about half as fast.
- Built with clang and tuned for the CPU, ggml's kernels match candle's: within 10% either way, a little slower on one thread and a little faster on eight.
- This compares kernels, not models. ggml runs a model as one graph, and candle runs each operation separately.

In the whole model, on 4 fast cores, the compiler matters less than in the kernels, but it shows:

| Design | Microsoft's compiler: final (p90), busy | With AVX-VNNI | clang: final (p90), busy |
| --- | --- | --- | --- |
| Ultra cut at pauses, Silero check | 0.25 s (0.57 s), 38% | 0.22 s (0.54 s), 36% | 0.17 s (0.46 s), 33% |
| Hybrid, Silero check | 0.26 s (0.40 s), 20% | 0.21 s (0.35 s), 19% | 0.18 s (0.29 s), 16% |
| Nemotron, 160 ms chunks | 0.06 s (0.10 s), 13% | 0.06 s (0.10 s), 13% | 0.06 s (0.10 s), 12% |

### One design for every CPU

On the slow CPU, Ultra cut at pauses uses most of its time to transcribe the current segment again for the live text. Doing it less often frees the CPU:

| Design, slow CPU | Live text every | Live WER | Final (p90) | Delay | Busy |
| --- | --- | --- | --- | --- | --- |
| Ultra cut at pauses | 0.5 s | 20.9% | 1.44 s (2.37 s) | 0.57 s | 92% |
| Ultra cut at pauses | 1 s | 24.7% | 1.29 s (1.85 s) | 0.73 s | 64% |
| Ultra cut at pauses | 2 s | 29.1% | 0.99 s (1.83 s) | 1.27 s | 42% |
| Hybrid | 0.2 s | 20.2% | 1.02 s (1.41 s) | 0.48 s | 55% |

With a 1 s step, Ultra alone stays within 1 to 2 s on the slow CPU. The hybrid gives smoother live text there, at the cost of a second model: 730 MB more to download and about 600 MB more memory. Its live text is also rougher (18.8% against 11.6% on the fast CPU), and it changes at each pause when Ultra's text replaces Nemotron's.

## Findings

- **The level matters.** Nemotron returned nothing on the quietest recording. The app must even out the microphone's level before the model.
- **Cutting at pauses makes an accurate offline model usable live.** Its final text stays fast whatever the length of the utterance.
- **Silero belongs on the finished segments.** Finding the pauses with Silero delayed the final text (0.44 s instead of 0.25 s on the fast CPU, 2.20 s instead of 1.44 s on the slow one). Checking each segment costs nothing measurable, and it stops a model with hints from writing them out on silence.
- **Whisper needs a GPU.** On the CPU, its final text takes 7.8 s: it always computes 30 s windows.
- **The GPU removes every limit.** The most accurate designs, with hints, give their final text in 0.1 to 0.2 s and leave the CPU almost idle.
- **On Windows, clang builds faster ggml code than Microsoft's compiler**: 13 to 20% less CPU in the whole model.
- **One model can serve every CPU** if the live text's step adapts to the machine: 0.5 s on a fast CPU, 1 s on a slow one.

## Build, packaging and supply chain

- **transcribe.cpp** pulls 19 Rust crates. Its C++ source ships in the crate, and it builds with cmake and a C++ compiler, with no download. The first build takes about 1 min 20 s. On Windows, the crate builds with Microsoft's compiler unless `CC`, `CXX` and `CMAKE_GENERATOR` point cmake at clang-cl.
- **Skipping the C++ build.** With `TRANSCRIBE_DIR` set to a copy of the library installed by cmake, `cargo build` only links it. CI or Nix would build that copy once per version, and nobody else's build would need a C++ compiler. The `shared` feature does not help: it still compiles the C++.
- **Vulkan.** The Vulkan SDK is needed only to build: it holds the headers, the shader compiler and the import library. Users need only the Vulkan loader, `vulkan-1.dll`, which GPU drivers install. A static build imports `vulkan-1.dll` directly, so the app would not start on a machine without it. A build with `dynamic-backends` loads the Vulkan module only when it can.
- **CPU tuning.** By default, ggml is tuned for the CPU of the machine that builds it, so a release could crash on an older CPU. `dynamic-backends` ships one CPU module per processor generation and picks one at run time. The project's own Windows bundle shows the cost: 56 MB in all, of which the Vulkan module is 44 MB and the nine CPU modules about 10 MB.
- **C++ runtime.** transcribe.cpp adds `MSVCP140.dll` to the DLLs the Windows app needs, besides the `VCRUNTIME140.dll` it already needs. Both come with Microsoft's Visual C++ Redistributable.
- **Maintenance.** transcribe.cpp is young and mostly the work of one author. The crate pins the C++ source, and the MIT license allows a fork.
- **ONNX Runtime.** pyke's prebuilt static library, which `ort` downloads, did not link with the Visual C++ installed here. The bench loads Microsoft's signed DLL instead, taken from the PyPI wheel after a hash check.
- **burn's CubeCL CPU backend** compiles its kernels at run time with LLVM, a large C++ library fetched during the build.

## A Rust port

transcribe.cpp's code for what the app needs is about 12,000 to 15,000 lines of C++: the Parakeet family (10,400, with long-form and multi-speaker code that could stay out), the shared encoder (1,900), the audio features (1,200) and Silero (1,300).
candle already has ggml's 8-bit formats and fast CPU kernels in Rust, and reads GGUF files, so a port would rewrite only the model code. The bench can check a port: same files, same recordings, same text.
candle has no Vulkan backend, so such a port would run on the CPU on most laptops, where Intel and AMD graphics are reached only through Vulkan.

## Recommendation

- Build on transcribe.cpp, with `dynamic-backends` for releases, and with clang on Windows.
- Even out the level, cut at pauses by level, and let Silero check each segment.
- Without a usable GPU: Ultra cut at pauses, with the live text's step set by the machine's speed. The hybrid stays an option for very slow CPUs.
- With a GPU: Qwen3 or Whisper, with the project's terms as hints.
- Code terms on the CPU: a list of replacements per user first ("Tokyo" to "tokio"), and later the cleanup pass by an AI model.

## Open questions

- Why ggml's Nemotron is less accurate than ONNX Runtime's.
- How the app gets the hints: the project's crates, files and identifiers, or a list the user's own agent writes.
- Linux and macOS: not measured yet.
