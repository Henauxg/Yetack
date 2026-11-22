//! Talk to one engine through the microphone and watch the text come.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bench_core::args::Args;
use bench_core::audio::Resampler;
use bench_core::ms;
use bench_core::protocol::{Event, Frame, SAMPLE_RATE};

use crate::manifest::{Configs, Models, Prompts, load};
use crate::mic::{Mic, meter};
use crate::process::EngineProcess;
use crate::stdin_lines;

pub fn live(data: &Path, args: &Args) -> Result<()> {
    let configs: Configs = load("configs.toml")?;
    let models: Models = load("models.toml")?;
    let config = configs.find(args.required("config")?)?;
    let model = models.find(&config.model)?;
    if !model.downloaded(data) {
        bail!("model {} is not downloaded: voice-bench fetch {}", model.id, model.id);
    }
    let threads: usize = args.or("threads", 4)?;
    let mut extra = Vec::new();
    if args.has("terms") {
        let prompts: Prompts = load("prompts.toml")?;
        let terms: Vec<&str> = prompts.prompt.iter().flat_map(|p| p.terms.iter().map(String::as_str)).collect();
        let path = data.join("terms.txt");
        std::fs::write(&path, terms.join("\n"))?;
        extra.extend(["--terms".to_owned(), path.display().to_string()]);
    }
    println!("Loading {}...", config.name);
    let mut engine = EngineProcess::start(data, config, model, threads, &extra)?;
    let mic = Mic::open()?;
    println!(
        "{} on {}, {}, loaded in {:.1} s. Microphone: {} at {} Hz.",
        engine.ready.model, engine.ready.backend, engine.ready.strategy, engine.ready.load_ms / 1000.0, mic.name, mic.rate
    );
    let lines = stdin_lines();
    loop {
        print!("\nEnter: talk, then Enter to stop. q: quit > ");
        std::io::stdout().flush()?;
        if lines.recv()?.trim() == "q" {
            break;
        }
        mic.start();
        let start = Instant::now();
        let mut resampler = Resampler::new(mic.rate, SAMPLE_RATE);
        let mut shown = String::new();
        let mut lag = 0.0;
        loop {
            let mut pcm = Vec::new();
            resampler.process(&mic.take(), &mut pcm);
            if !pcm.is_empty() {
                engine.send(&Frame::Audio(pcm))?;
            }
            while let Ok((at, event)) = engine.events.try_recv() {
                if let Event::Partial { text, input_ms, .. } = event {
                    lag = ms(at - start) - input_ms;
                    shown = text;
                }
            }
            let tail: String = shown.chars().rev().take(70).collect::<Vec<_>>().into_iter().rev().collect();
            print!("\r\x1b[2K{} {:>5.0} ms | {tail}", meter(mic.level()).chars().take(10).collect::<String>(), lag);
            std::io::stdout().flush()?;
            if lines.try_recv().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut pcm = Vec::new();
        resampler.process(&mic.stop(), &mut pcm);
        resampler.flush(&mut pcm);
        engine.send(&Frame::Audio(pcm))?;
        let released = Instant::now();
        engine.send(&Frame::End)?;
        loop {
            let (at, event) = engine.events.recv_timeout(Duration::from_secs(300)).context("no final text")?;
            match event {
                Event::Final { text, usage, .. } => {
                    println!(
                        "\r\x1b[2K\nFinal, {:.0} ms after release ({:.0} MB peak):\n  {text}",
                        ms(at - released),
                        usage.peak_working_set as f64 / 1e6
                    );
                    break;
                }
                Event::Error { message } => {
                    println!("\nerror: {message}");
                    break;
                }
                _ => {}
            }
        }
    }
    engine.quit();
    Ok(())
}
