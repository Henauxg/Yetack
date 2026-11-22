//! models.toml, configs.toml and prompts.toml, and the download of models.

use std::fs::{self, File};
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::bench_dir;

#[derive(Deserialize)]
pub struct Models {
    pub model: Vec<Model>,
}

#[derive(Deserialize, Clone)]
pub struct Model {
    pub id: String,
    pub repo: String,
    pub revision: String,
    /// The engine loads the folder of the files rather than the one file.
    pub folder: bool,
    pub license: String,
    pub note: String,
    pub file: Vec<ModelFile>,
}

#[derive(Deserialize, Clone)]
pub struct ModelFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
pub struct Configs {
    pub config: Vec<Config>,
}

#[derive(Deserialize, Clone)]
pub struct Config {
    pub name: String,
    pub engine: String,
    pub model: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// A second model, for the final text.
    #[serde(default)]
    pub final_model: Option<String>,
    /// A voice detection model, for the pauses.
    #[serde(default)]
    pub vad_model: Option<String>,
}

#[derive(Deserialize)]
pub struct Prompts {
    pub prompt: Vec<Prompt>,
}

#[derive(Deserialize, Clone)]
pub struct Prompt {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub terms: Vec<String>,
}

pub fn load<T: DeserializeOwned>(name: &str) -> Result<T> {
    let path = bench_dir().join(name);
    let text = fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("{}", path.display()))
}

impl Model {
    pub fn dir(&self, data: &Path) -> PathBuf {
        data.join("models").join(&self.id)
    }

    fn local(&self, data: &Path, file: &ModelFile) -> PathBuf {
        let name = Path::new(&file.path).file_name().map(PathBuf::from).unwrap_or_default();
        self.dir(data).join(name)
    }

    /// What the engine loads: the model's file, or its folder.
    pub fn load_path(&self, data: &Path) -> PathBuf {
        match (self.folder, self.file.first()) {
            (false, Some(file)) => self.local(data, file),
            _ => self.dir(data),
        }
    }

    pub fn downloaded(&self, data: &Path) -> bool {
        self.file.iter().all(|file| verified(&self.local(data, file), file))
    }

    pub fn size(&self) -> u64 {
        self.file.iter().map(|file| file.size).sum()
    }
}

impl Configs {
    pub fn find(&self, name: &str) -> Result<&Config> {
        self.config
            .iter()
            .find(|config| config.name == name)
            .with_context(|| format!("no config {name} in configs.toml"))
    }
}

impl Models {
    pub fn find(&self, id: &str) -> Result<&Model> {
        self.model
            .iter()
            .find(|model| model.id == id)
            .with_context(|| format!("no model {id} in models.toml"))
    }
}

pub fn list_models(data: &Path) -> Result<()> {
    let models: Models = load("models.toml")?;
    for model in &models.model {
        let state = if model.downloaded(data) { "here" } else { "    " };
        println!(
            "{state}  {:22} {:>6} MB  {:28} {}",
            model.id,
            model.size() / 1_000_000,
            model.license,
            model.note
        );
    }
    println!("\nModels go to {}", data.join("models").display());
    Ok(())
}

pub fn list_configs() -> Result<()> {
    let configs: Configs = load("configs.toml")?;
    for config in &configs.config {
        println!("{:28} {:22} {:18} {}", config.name, config.engine, config.model, config.args.join(" "));
    }
    Ok(())
}

/// Downloads with curl, from the pinned commit, and keeps a file only if its SHA-256 matches.
pub fn fetch(data: &Path, ids: &[String]) -> Result<()> {
    let models: Models = load("models.toml")?;
    let wanted: Vec<&Model> = if ids.iter().any(|id| id == "all") {
        models.model.iter().collect()
    } else if ids.is_empty() {
        bail!("name the models to fetch, or all");
    } else {
        ids.iter().map(|id| models.find(id)).collect::<Result<_>>()?
    };
    for model in wanted {
        fs::create_dir_all(model.dir(data))?;
        for file in &model.file {
            let target = model.local(data, file);
            if verified(&target, file) {
                println!("{}: {} is here", model.id, file.path);
                continue;
            }
            let partial = PathBuf::from(format!("{}.part", target.display()));
            let url = format!("https://huggingface.co/{}/resolve/{}/{}", model.repo, model.revision, file.path);
            println!("{}: {} ({} MB)", model.id, file.path, file.size / 1_000_000);
            let status = Command::new("curl")
                .args(["--fail", "--location", "--retry", "3", "--continue-at", "-", "--output"])
                .arg(&partial)
                .arg(&url)
                .status()
                .context("running curl")?;
            if !status.success() {
                bail!("curl failed for {url}");
            }
            let hash = sha256(&partial)?;
            if hash != file.sha256 {
                fs::remove_file(&partial)?;
                bail!("{}: SHA-256 {hash}, expected {}; the file is deleted", file.path, file.sha256);
            }
            fs::rename(&partial, &target)?;
            fs::write(marker(&target), &file.sha256)?;
        }
    }
    Ok(())
}

/// A file counts as checked when its size matches and a marker beside it holds its hash.
fn verified(path: &Path, file: &ModelFile) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.len() == file.size)
        && fs::read_to_string(marker(path)).is_ok_and(|hash| hash.trim() == file.sha256)
}

fn marker(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.sha256", path.display()))
}

fn sha256(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}
