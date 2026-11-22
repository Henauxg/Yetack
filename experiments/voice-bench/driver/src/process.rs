//! An engine binary run as a child process, with its events timestamped on arrival.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bench_core::protocol::{self, Event, Frame, Ready};

use crate::manifest::{Config, Model, Models, load};

pub struct EngineProcess {
    child: Child,
    stdin: ChildStdin,
    pub events: Receiver<(Instant, Event)>,
    pub ready: Ready,
    pub log: PathBuf,
}

impl EngineProcess {
    pub fn start(data: &Path, config: &Config, model: &Model, threads: usize, extra: &[String]) -> Result<Self> {
        Self::start_on(data, config, model, threads, extra, None)
    }

    /// `affinity` is a mask of the logical processors the engine may run on.
    pub fn start_on(
        data: &Path,
        config: &Config,
        model: &Model,
        threads: usize,
        extra: &[String],
        affinity: Option<u64>,
    ) -> Result<Self> {
        let exe = engine_path(&config.engine)?;
        let logs = data.join("logs");
        std::fs::create_dir_all(&logs)?;
        let log = logs.join(format!("{}.log", config.name));
        let stderr = File::create(&log)?;
        let mut final_model = Vec::new();
        for (flag, id) in [("--final-model", &config.final_model), ("--vad", &config.vad_model)] {
            let Some(id) = id else { continue };
            let models: Models = load("models.toml")?;
            let model = models.find(id)?;
            if !model.downloaded(data) {
                bail!("model {id} is not downloaded: voice-bench fetch {id}");
            }
            final_model.push(flag.into());
            final_model.push(model.load_path(data).into_os_string());
        }
        let mut child = Command::new(&exe)
            .arg("--model")
            .arg(model.load_path(data))
            .arg("--threads")
            .arg(threads.to_string())
            .args(&config.args)
            .args(&final_model)
            .args(extra)
            .env("RAYON_NUM_THREADS", threads.to_string())
            .env("ORT_DYLIB_PATH", onnx_runtime(data))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        if let Some(mask) = affinity {
            set_affinity(&child, mask)?;
        }
        let stdin = child.stdin.take().context("engine stdin")?;
        let stdout = child.stdout.take().context("engine stdout")?;
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let at = Instant::now();
                if let Ok(event) = serde_json::from_str::<Event>(&line)
                    && sender.send((at, event)).is_err()
                {
                    break;
                }
            }
        });
        let ready = loop {
            match events.recv_timeout(Duration::from_secs(900)) {
                Ok((_, Event::Ready(ready))) => break ready,
                Ok((_, Event::Error { message })) => bail!("{}: {message}", config.name),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => bail!("{} did not load in 15 minutes", config.name),
                Err(RecvTimeoutError::Disconnected) => {
                    let tail = log_tail(&log);
                    bail!("{} exited while loading; {}:\n{tail}", config.name, log.display())
                }
            }
        };
        Ok(Self { child, stdin, events, ready, log })
    }

    pub fn send(&mut self, frame: &Frame) -> Result<()> {
        protocol::write_frame(&mut self.stdin, frame).context("writing to the engine")
    }

    pub fn quit(mut self) {
        let _ = self.send(&Frame::Quit);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
    }
}

/// Microsoft's ONNX Runtime, taken from its PyPI wheel (README.md says how).
fn onnx_runtime(data: &Path) -> PathBuf {
    let name = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };
    data.join("runtime").join("onnxruntime-1.31.1").join("dll").join(name)
}

fn engine_path(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("the driver's folder")?;
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        bail!("{} is missing: cargo build --release -p {name}", path.display());
    }
    Ok(path)
}

#[cfg(windows)]
fn set_affinity(child: &Child, mask: u64) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    // SAFETY: the handle belongs to the child, which is alive.
    let done = unsafe {
        windows_sys::Win32::System::Threading::SetProcessAffinityMask(child.as_raw_handle(), mask as usize)
    };
    if done == 0 {
        bail!("setting the affinity: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_affinity(_: &Child, _: u64) -> Result<()> {
    bail!("--affinity works on Windows only; use taskset")
}

pub fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}
