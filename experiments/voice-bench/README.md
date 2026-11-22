# Voice bench

A bench of local speech-to-text engines for dictation in the desktop app.
It plays recordings into each engine at the pace of speech, as a microphone would, and measures what the user would see: how wrong the text is, how far the live text trails the voice, how long the final text takes after the key is released, and what the engine costs in CPU and memory.

It is its own Cargo workspace, so the app's `just check` never builds its C++ and ONNX dependencies.

## What it compares

| Runtime | Crate | Models | Build | Rust crates |
| --- | --- | --- | --- | --- |
| ggml (C++) | `transcribe-cpp` 0.4.2 | Nemotron streaming, Parakeet unified, Parakeet TDT v3, Parakeet Ultra, Whisper turbo, Moonshine, Qwen3-ASR | cmake and a C++ compiler. The C++ source ships in the crate. Nothing downloads. | 19 |
| ONNX Runtime | `parakeet-rs` 0.4.4 | Nemotron streaming, Parakeet unified | Loads Microsoft's `onnxruntime.dll` at run time. Nothing downloads during the build. | 217, with burn |
| burn (pure Rust) | `parakeet-rs` 0.4.4 | the same ONNX files | Rust only, but it pulls the Oniguruma C library through `tokenizers`. | the same binary |

Each engine is a separate binary that the driver runs as a child process.
So each engine's memory is measured alone, and two native runtimes never link into one binary.

`configs.toml` lists the combinations, `models.toml` pins each model to a Hugging Face commit with the SHA-256 of every file, and `prompts.toml` holds the prompts to read.

## Build

On Windows, with Visual Studio's C++ tools and cmake:

```sh
cargo build --release
```

The ONNX engine needs Microsoft's ONNX Runtime 1.31.1.
`scripts/get_onnx_runtime.py` takes it from Microsoft's PyPI wheel, checks the wheel against the SHA-256 that PyPI publishes, and copies out the DLLs:

```sh
python -I scripts/get_onnx_runtime.py 1.31.1 <an empty folder for the wheel> <data>/runtime/onnxruntime-1.31.1/dll
```

The driver passes that path to the engine in `ORT_DYLIB_PATH`.

On Windows, transcribe.cpp builds with Microsoft's compiler, whose ggml code is slower than clang's. To build the engine with clang-cl, from a Visual Studio x64 developer prompt with LLVM on the `PATH`:

```bat
set CMAKE_GENERATOR=NMake Makefiles
set CC=clang-cl
set CXX=clang-cl
set CFLAGS=/clang:-march=native
set CXXFLAGS=/clang:-march=native
set TRANSCRIBE_CMAKE_ARGS=-DGGML_AVX_VNNI=ON -DGGML_BMI2=ON
cargo build --release -p engine_transcribe_cpp --target-dir target-clang
```

`-march=native` tunes the code for the CPU that builds it, so the binary may not run on an older one.
For the Vulkan backend, add `--features vulkan`, with the Vulkan SDK installed and its `Lib` folder in the `LIB` variable.

`kernels/` times the matrix products of Parakeet's encoder: `kernels/candle` with candle, and `kernels/ggml` with the ggml that transcribe-cpp-sys ships (its `CMakeLists.txt` says how to build it).
`scripts/pin_models.py` writes `models.toml` again, from the models' current Hugging Face commits.

## Use

`<data>` is `%LOCALAPPDATA%\yetack-voice-bench` on Windows, `~/.local/share/yetack-voice-bench` elsewhere, or the folder in `VOICE_BENCH_DATA` or `--data`.
Models, recordings and results all go there, outside the repository.

```sh
voice-bench models                      # what is downloaded
voice-bench fetch all                   # about 10 GB; or name the models
voice-bench record --set me             # read the 24 prompts into the microphone
voice-bench run --set me --config all   # every configuration on your recordings
voice-bench live --config ggml-nemotron-en-160ms   # talk to one engine and watch the text
```

`voice-bench tts --set tts` makes recordings with Windows' speech synthesis.
Its voice is hard to understand, so use them to test the pipeline, not to judge accuracy.

Options of `run`:

- `--threads 4`: the threads each engine may use.
- `--affinity <hex mask>`: the logical processors the engine may run on (Windows). On an Intel CPU with efficiency cores, `F0000` gives four efficiency cores, a stand-in for a slower laptop. `55` gives four performance cores, one thread each.
- `--normalize`: scales each recording to a peak of 0.7, as the app's level control would. Nemotron misses quiet speech without it.
- `--terms`: passes every prompt's terms as vocabulary hints, to the models that take them.
- `--only 01-check,24-reconnect`: some of the recordings.
- `--pace fast`: sends the audio as fast as the engine takes it. The delays then mean nothing, but the run is quicker.

Each run prints a table, writes it as Markdown to `<data>/results/`, and appends every detail, down to each live text and when it arrived, to `<data>/results/results.jsonl`.

## What the numbers mean

- **WER**: the word error rate of the final text against what you read, after both are normalized (case, punctuation, "dot" and "slash", numbers as words, filler words).
- **Live WER**: the same for the last live text before the release of the key.
- **Terms**: the share of the prompts' code terms that come out recognizable, in any spacing or punctuation.
- **Final after release**: from the release of the key to the final text.
- **Text delay**: for each word you say, from its end to when the text on screen has that many words. The word times come from one offline pass of Parakeet TDT v3 with word timestamps, kept in the set's `word-ends.json`.
- **Updates every**: the time between two changes of the live text.
- **First text**: from the start of speech to the first live text.
- **Busy**: the engine's compute time over the audio's length. Above 100%, it cannot keep up.
- **Cores**: the engine's CPU time over the wall time, while the utterances run.
- **Peak RAM (private)**: the peak working set, which counts the pages of a memory-mapped model file, and the peak memory the engine allocated itself.

## How live text is made

- `stream`: the model streams. Nemotron is cache-aware: it computes each slice of audio once. Parakeet unified buffers: it computes a few seconds of context again at each step.
- `rerun`: an offline model transcribes the whole utterance again every step (500 ms or 1 s). It costs more as the utterance grows.
- `segmented`: an offline model cuts the utterance at pauses of 400 ms. It transcribes each finished segment once, while the user goes on, and the live text comes from transcribing only the current segment again. At the release of the key, only the last segment is left. The pauses come from the audio's level (a tenth of the loudest 20 ms frame so far), not from a voice detection model.
- `offline`: text only at the end.
- `--final-pass` (in `args`): live text from the stream, and the final text from one offline pass over the whole utterance.
- `final_model` (in a config): live text from the streaming model, and the final text from the second model, segment by segment between pauses, as `segmented` does.

Parakeet Ultra and Parakeet TDT v3 do not stream in transcribe.cpp: its buffered streaming returns "not implemented" for them.
