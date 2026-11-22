//! Dictation in a terminal, through the default microphone, with Parakeet unified EN:
//! `cargo run --release -p cockpit_voice --example dictate -- <models folder>`.
//! Enter starts a recording, Enter again stops it, and `q` then Enter quits.

use std::error::Error;
use std::io::{self, BufRead};
use std::path::PathBuf;

use cockpit_voice::{Event, RecordingOptions, Voice};

fn main() -> Result<(), Box<dyn Error>> {
    let Some(models) = std::env::args().nth(1) else {
        eprintln!("usage: dictate <models folder>");
        return Ok(());
    };
    if !cockpit_voice::supported() {
        eprintln!("this CPU cannot run the speech model");
        return Ok(());
    }
    let voice = Voice::spawn(PathBuf::from(models))?;
    let mut recording = false;
    println!("Enter: start talking, Enter again: stop, q: quit");
    for line in io::stdin().lock().lines() {
        if line?.trim() == "q" {
            break;
        }
        if recording {
            voice.stop()?;
        } else {
            voice.start(RecordingOptions::default(), |event| match event {
                Event::Level(_) => {}
                Event::Live(text) => println!("  {} [{}]", text.said, text.hearing),
                Event::Fallback { chosen, using, .. } => {
                    println!("{chosen} is missing: {}", using.unwrap_or_default());
                }
                Event::Finishing => println!("  ..."),
                Event::Done(text) => println!("final: {text}"),
                Event::Cancelled => println!("cancelled"),
                Event::Failed(error) => println!("failed: {error}"),
            })?;
        }
        recording = !recording;
    }
    Ok(())
}
