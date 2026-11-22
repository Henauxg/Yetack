//! Runs NVIDIA's streaming models with parakeet-rs, on ONNX Runtime or on burn (pure Rust),
//! behind the bench's pipe protocol.
//!
//! `--model <folder> --kind nemotron|unified [--backend ort|burn] [--threads 4] [--final-pass]`

use std::time::Instant;

use anyhow::{Result, bail};
use bench_core::args::Args;
use bench_core::engine::{self, Engine, Utterance};
use bench_core::protocol::{Ready, SAMPLE_RATE};
use parakeet_rs::{ExecutionConfig, ExecutionProvider, Nemotron, ParakeetUnified};

fn main() -> Result<()> {
    let args = Args::from_env();
    let started = Instant::now();
    let path = args.required("model")?;
    let threads: usize = args.or("threads", 4)?;
    let backend = args.get("backend").unwrap_or("ort");
    let config = match backend {
        "ort" => ExecutionConfig::new()
            .with_environment(parakeet_rs::ort::init().build()?)
            .with_execution_provider(ExecutionProvider::Cpu)
            .with_intra_threads(threads)
            .with_inter_threads(1)
            // Idle worker threads spin by default, which burns CPU between chunks.
            .with_custom_configure(|builder| {
                let builder = builder.with_intra_op_spinning(false).unwrap_or_else(|e| e.recover());
                Ok(builder.with_inter_op_spinning(false).unwrap_or_else(|e| e.recover()))
            }),
        "burn" => ExecutionConfig::new().with_execution_provider(ExecutionProvider::BurnCpu),
        #[cfg(feature = "wgpu")]
        "burn-wgpu" => ExecutionConfig::new().with_execution_provider(ExecutionProvider::BurnWgpu),
        other => bail!("unknown backend {other}"),
    };
    let final_pass = args.has("final-pass");
    let model_name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned());
    let mut ready = Ready {
        engine: "parakeet-rs".to_owned(),
        model: model_name,
        backend: match backend {
            "ort" => "ONNX Runtime CPU",
            "burn" => "burn CPU",
            _ => "burn wgpu",
        }
        .to_owned(),
        threads,
        strategy: "stream".to_owned(),
        ..Default::default()
    };
    match args.required("kind")? {
        "nemotron" => {
            let model = Nemotron::from_pretrained(path, Some(config))?;
            ready.settings = format!("chunk {} ms", model.chunk_samples() * 1000 / SAMPLE_RATE as usize);
            engine::serve(ready, started, &mut NemotronEngine { model })
        }
        "unified" => {
            let model = ParakeetUnified::from_pretrained(path, Some(config))?;
            let window = model.streaming_config();
            ready.settings = format!(
                "buffer {:.0}/{:.0}/{:.0} ms{}",
                window.left_context_secs * 1000.0,
                window.chunk_secs * 1000.0,
                window.right_context_secs * 1000.0,
                if final_pass { ", offline final pass" } else { "" }
            );
            engine::serve(ready, started, &mut UnifiedEngine { model, final_pass })
        }
        other => bail!("unknown kind {other}"),
    }
}

struct NemotronEngine {
    model: Nemotron,
}

impl Engine for NemotronEngine {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        self.model.reset();
        // The model takes one chunk per call, so it gets exactly one each time.
        let chunk = self.model.chunk_samples();
        let mut pending = Vec::new();
        while let Some(pcm) = utterance.audio() {
            pending.extend_from_slice(&pcm);
            let mut changed = false;
            while pending.len() >= chunk {
                let piece: Vec<f32> = pending.drain(..chunk).collect();
                changed |= !self.model.transcribe_chunk(&piece)?.is_empty();
            }
            if changed {
                utterance.text(&self.model.get_transcript());
            }
        }
        if !pending.is_empty() {
            pending.resize(chunk, 0.0);
            self.model.transcribe_chunk(&pending)?;
        }
        // One chunk of silence lets the last words out.
        self.model.transcribe_chunk(&vec![0.0; chunk])?;
        Ok(self.model.get_transcript())
    }
}

struct UnifiedEngine {
    model: ParakeetUnified,
    final_pass: bool,
}

impl Engine for UnifiedEngine {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        self.model.reset();
        let mut all = Vec::new();
        while let Some(pcm) = utterance.audio() {
            if self.final_pass {
                all.extend_from_slice(&pcm);
            }
            if !self.model.transcribe_chunk(&pcm)?.is_empty() {
                utterance.text(&self.model.get_transcript());
            }
        }
        self.model.flush()?;
        if self.final_pass && !all.is_empty() {
            return Ok(self.model.transcribe_audio(all, SAMPLE_RATE, 1)?);
        }
        Ok(self.model.get_transcript())
    }
}
