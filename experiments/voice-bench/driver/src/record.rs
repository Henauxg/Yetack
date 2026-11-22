//! Recording the prompts, through the microphone or Windows' speech synthesis.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bench_core::args::Args;
use bench_core::audio::{read_wav, resample_all, write_wav};
use bench_core::protocol::SAMPLE_RATE;

use crate::manifest::{Prompts, load};
use crate::mic::{Mic, meter};
use crate::stdin_lines;

pub fn record(data: &Path, args: &Args) -> Result<()> {
    let set = args.required("set")?;
    let dir = data.join("recordings").join(set);
    std::fs::create_dir_all(&dir)?;
    let prompts: Prompts = load("prompts.toml")?;
    let only: Option<Vec<&str>> = args.get("only").map(|ids| ids.split(',').collect());
    let mic = Mic::open()?;
    println!("Microphone: {} at {} Hz", mic.name, mic.rate);
    println!("Recordings go to {}", dir.display());
    println!("Read each prompt as you would talk to an agent. Pauses and restarts are fine.");
    let lines = stdin_lines();
    let total = prompts.prompt.len();
    for (index, prompt) in prompts.prompt.iter().enumerate() {
        if only.as_ref().is_some_and(|ids| !ids.contains(&prompt.id.as_str())) {
            continue;
        }
        let path = dir.join(format!("{}.wav", prompt.id));
        if path.exists() && !args.has("again") {
            println!("{}: recorded already (--again records it again)", prompt.id);
            continue;
        }
        loop {
            println!("\n[{}/{total}] {}\n\n    {}\n", index + 1, prompt.id, prompt.text);
            print!("Enter: start, s: skip, q: quit > ");
            std::io::stdout().flush()?;
            match lines.recv()?.trim() {
                "q" => return Ok(()),
                "s" => break,
                _ => {}
            }
            mic.start();
            let started = Instant::now();
            loop {
                match lines.recv_timeout(Duration::from_millis(100)) {
                    Ok(_) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        print!(
                            "\r  recording {:5.1} s  {}  Enter: stop ",
                            started.elapsed().as_secs_f32(),
                            meter(mic.level())
                        );
                        std::io::stdout().flush()?;
                    }
                    Err(RecvTimeoutError::Disconnected) => bail!("the terminal closed"),
                }
            }
            let pcm = resample_all(&mic.stop(), mic.rate, SAMPLE_RATE);
            let peak = pcm.iter().fold(0.0f32, |a, s| a.max(s.abs()));
            println!("Recorded {:.1} s, peak {peak:.2}", pcm.len() as f32 / SAMPLE_RATE as f32);
            if peak < 0.02 {
                println!("That is almost silent: check the microphone.");
            }
            print!("Enter: keep, r: record again > ");
            std::io::stdout().flush()?;
            if lines.recv()?.trim() == "r" {
                continue;
            }
            write_wav(&path, &pcm)?;
            break;
        }
    }
    println!("\nDone: {}", dir.display());
    Ok(())
}

/// The prompts spoken by Windows' speech synthesis, with silence around them.
pub fn tts(data: &Path, args: &Args) -> Result<()> {
    let set = args.get("set").unwrap_or("tts");
    let dir = data.join("recordings").join(set);
    std::fs::create_dir_all(&dir)?;
    let prompts: Prompts = load("prompts.toml")?;
    let script = "Add-Type -AssemblyName System.Speech; \
        $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
        $f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, \
            [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, \
            [System.Speech.AudioFormat.AudioChannel]::Mono); \
        $s.SetOutputToWaveFile($env:VB_PATH, $f); $s.Speak($env:VB_TEXT); $s.Dispose()";
    for prompt in &prompts.prompt {
        let path = dir.join(format!("{}.wav", prompt.id));
        if path.exists() && !args.has("again") {
            continue;
        }
        let raw = dir.join(format!("{}.raw.wav", prompt.id));
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("VB_PATH", &raw)
            .env("VB_TEXT", &prompt.text)
            .status()
            .context("running PowerShell")?;
        if !status.success() {
            bail!("speech synthesis failed for {}", prompt.id);
        }
        let speech = read_wav(&raw)?;
        let mut pcm = vec![0.0; SAMPLE_RATE as usize * 4 / 10];
        pcm.extend_from_slice(&speech);
        pcm.extend(std::iter::repeat_n(0.0, SAMPLE_RATE as usize * 6 / 10));
        write_wav(&path, &pcm)?;
        std::fs::remove_file(&raw)?;
        println!("{}: {:.1} s", prompt.id, pcm.len() as f32 / SAMPLE_RATE as f32);
    }
    Ok(())
}
