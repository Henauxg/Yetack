//! The voice bench's driver: it fetches models, records prompts, runs each engine on the
//! recordings at the pace of speech, and reports accuracy, delays, CPU and memory.

mod bench;
mod live;
mod manifest;
mod mic;
mod process;
mod record;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result, bail};
use bench_core::args::Args;

const HELP: &str = "voice-bench <command>

  models                       the models, their size, and whether they are downloaded
  fetch <model id>... | all    downloads models and checks each file's SHA-256
  configs                      the bench configurations
  record --set <name> [--only <id,...>] [--again]
                               read the prompts into the microphone
  tts --set <name> [--again]   recordings made by Windows' speech synthesis, for smoke tests
  run --set <name> --config <name,...|all> [--threads 4] [--pace realtime|fast]
      [--chunk-ms 100] [--terms] [--only <id,...>] [--affinity <hex mask>]
      [--normalize] [--level-control] [--engine-args='--backend vulkan']
                               runs engines on a set of recordings and reports the results
  live --config <name> [--threads 4] [--terms]
                               talk to one engine through the microphone

  --data <dir>  where models, recordings and results go: VOICE_BENCH_DATA by default, or
                yetack-voice-bench in the local data folder";

fn main() -> Result<()> {
    let args = Args::from_env();
    let Some(command) = args.positional.first().cloned() else {
        println!("{HELP}");
        return Ok(());
    };
    let data = data_dir(&args)?;
    match command.as_str() {
        "models" => manifest::list_models(&data),
        "fetch" => manifest::fetch(&data, &args.positional[1..]),
        "configs" => manifest::list_configs(),
        "record" => record::record(&data, &args),
        "tts" => record::tts(&data, &args),
        "run" => bench::run(&data, &args),
        "live" => live::live(&data, &args),
        _ => {
            println!("{HELP}");
            bail!("unknown command {command}")
        }
    }
}

fn data_dir(args: &Args) -> Result<PathBuf> {
    if let Some(dir) = args.get("data") {
        return Ok(dir.into());
    }
    if let Some(dir) = std::env::var_os("VOICE_BENCH_DATA") {
        return Ok(dir.into());
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
    });
    Ok(base.context("no data folder: pass --data")?.join("yetack-voice-bench"))
}

/// The folder of models.toml, configs.toml and prompts.toml.
fn bench_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Lines typed on the terminal, read by a thread so the caller can wait with a timeout.
fn stdin_lines() -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}
