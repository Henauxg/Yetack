//! Runs engines on a set of recordings at the pace of speech, and measures what a user would
//! see: how wrong the text is, how far the live text trails the voice, how long the final text
//! takes after the key is released, and what the engine costs in CPU and memory.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bench_core::args::Args;
use bench_core::audio::{read_wav, speech_span_ms};
use bench_core::protocol::{Event, Frame, Ready, SAMPLE_RATE, Usage};
use bench_core::score::{has_term, normalize, word_errors};
use bench_core::ms;
use serde::Serialize;

use crate::manifest::{Config, Configs, Model, Models, Prompt, Prompts, load};
use crate::process::EngineProcess;

struct Clip {
    prompt: Prompt,
    pcm: Vec<f32>,
    onset_ms: f64,
    end_ms: f64,
    /// When each normalized word ends, from the aligner's word timestamps.
    word_ends_ms: Option<Vec<f64>>,
}

#[derive(Serialize)]
struct PartialRecord {
    /// From the start of the utterance.
    at_ms: f64,
    input_ms: f64,
    compute_ms: f64,
    text: String,
}

#[derive(Serialize)]
struct UtteranceResult {
    id: String,
    audio_ms: f64,
    onset_ms: f64,
    text: String,
    /// The last live text before the key was released.
    live_text: String,
    errors: usize,
    words: usize,
    live_errors: usize,
    terms_hit: usize,
    terms: usize,
    missed_terms: Vec<String>,
    /// From the release of the key to the final text.
    final_ms: f64,
    final_compute_ms: f64,
    /// From the start of speech to the first live text.
    first_text_ms: Option<f64>,
    /// How far behind the received audio each live text arrived: the engine's compute.
    lags_ms: Vec<f64>,
    /// For each word, from when it was said to when the text on screen had that many words.
    /// Words are taken as evenly spread over the speech.
    word_delays_ms: Vec<f64>,
    /// The time between two changes of the live text.
    update_ms: Vec<f64>,
    busy_ms: f64,
    cpu_ms: f64,
    wall_ms: f64,
    partials: Vec<PartialRecord>,
}

#[derive(Serialize, Clone)]
struct Summary {
    config: String,
    strategy: String,
    settings: String,
    backend: String,
    threads: usize,
    utterances: usize,
    wer: f64,
    live_wer: f64,
    terms: f64,
    final_ms_median: f64,
    final_ms_p90: f64,
    lag_ms_median: Option<f64>,
    lag_ms_p90: Option<f64>,
    delay_ms_median: Option<f64>,
    delay_ms_p90: Option<f64>,
    update_ms_median: Option<f64>,
    first_text_ms_median: Option<f64>,
    /// Engine compute time over audio time.
    busy: f64,
    /// CPU time over wall time while utterances run.
    cores: f64,
    load_ms: f64,
    peak_working_set_mb: f64,
    peak_private_mb: f64,
    model_mb: f64,
}

pub fn run(data: &Path, args: &Args) -> Result<()> {
    let set = args.required("set")?;
    let dir = data.join("recordings").join(set);
    let prompts: Prompts = load("prompts.toml")?;
    let models: Models = load("models.toml")?;
    let configs: Configs = load("configs.toml")?;
    let names = args.required("config")?;
    let selected: Vec<&Config> = if names == "all" {
        configs.config.iter().collect()
    } else {
        names.split(',').map(|name| configs.find(name)).collect::<Result<_>>()?
    };
    let threads: usize = args.or("threads", 4)?;
    let realtime = match args.get("pace").unwrap_or("realtime") {
        "realtime" => true,
        "fast" => false,
        other => bail!("unknown pace {other}"),
    };
    let chunk_ms: usize = args.or("chunk-ms", 100)?;
    let affinity = args
        .get("affinity")
        .map(|mask| u64::from_str_radix(mask.trim_start_matches("0x"), 16).context("--affinity <hex mask>"))
        .transpose()?;
    let only: Option<Vec<&str>> = args.get("only").map(|ids| ids.split(',').collect());

    let mut clips = Vec::new();
    for prompt in &prompts.prompt {
        if only.as_ref().is_some_and(|ids| !ids.contains(&prompt.id.as_str())) {
            continue;
        }
        let path = dir.join(format!("{}.wav", prompt.id));
        if !path.exists() {
            continue;
        }
        let mut pcm = read_wav(&path)?;
        if args.has("normalize") {
            normalize_level(&mut pcm);
        }
        if args.has("level-control") {
            control_level(&mut pcm);
        }
        let (onset_ms, end_ms) = speech_span_ms(&pcm);
        clips.push(Clip { prompt: prompt.clone(), pcm, onset_ms, end_ms, word_ends_ms: None });
    }
    if clips.is_empty() {
        bail!("no recordings in {}: record them first", dir.display());
    }
    align(data, &dir, &models, &mut clips)?;
    let audio_s: f64 = clips.iter().map(|clip| clip.pcm.len() as f64 / f64::from(SAMPLE_RATE)).sum();
    println!("{} recordings, {audio_s:.0} s of audio, {threads} threads, {} pace", clips.len(), if realtime { "real-time" } else { "fast" });

    let mut extra: Vec<String> = args.get("engine-args").unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    if args.has("terms") {
        let terms: BTreeSet<&str> = prompts.prompt.iter().flat_map(|p| p.terms.iter().map(String::as_str)).collect();
        let path = data.join("terms.txt");
        std::fs::write(&path, terms.into_iter().collect::<Vec<_>>().join("\n"))?;
        extra.extend(["--terms".to_owned(), path.display().to_string()]);
    }

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let results_dir = data.join("results");
    std::fs::create_dir_all(&results_dir)?;
    let mut summaries = Vec::new();
    for config in selected {
        let model = models.find(&config.model)?;
        if !model.downloaded(data) {
            println!("\n{}: skipped, model {} is not downloaded (voice-bench fetch {})", config.name, model.id, model.id);
            continue;
        }
        println!("\n{}", config.name);
        match bench_config(data, config, model, threads, affinity, &extra, &clips, realtime, chunk_ms) {
            Ok((ready, utterances, summary)) => {
                print_summary(&summary);
                let record = serde_json::json!({
                    "stamp": stamp, "set": set, "pace": if realtime { "realtime" } else { "fast" },
                    "chunk_ms": chunk_ms, "terms": args.has("terms"), "normalize": args.has("normalize"), "level_control": args.has("level-control"), "engine_args": args.get("engine-args"), "affinity": args.get("affinity"), "summary": summary,
                    "ready": ready, "utterances": utterances,
                });
                let mut file = OpenOptions::new().create(true).append(true).open(results_dir.join("results.jsonl"))?;
                writeln!(file, "{record}")?;
                summaries.push(summary);
            }
            Err(error) => println!("  failed: {error:#}"),
        }
    }
    if summaries.is_empty() {
        return Ok(());
    }
    let table = table(&summaries);
    println!("\n{table}");
    let report = results_dir.join(format!("{stamp}-{set}-{threads}t.md"));
    std::fs::write(&report, format!("Set {set}, {} recordings, {audio_s:.0} s, {threads} threads, {} pace, terms {}, normalize {}, level control {}\n\n{table}\n", clips.len(), if realtime { "real-time" } else { "fast" }, args.has("terms"), args.has("normalize"), args.has("level-control")))?;
    println!("Report: {}", report.display());
    println!("Details: {}", results_dir.join("results.jsonl").display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn bench_config(
    data: &Path,
    config: &Config,
    model: &Model,
    threads: usize,
    affinity: Option<u64>,
    extra: &[String],
    clips: &[Clip],
    realtime: bool,
    chunk_ms: usize,
) -> Result<(Ready, Vec<UtteranceResult>, Summary)> {
    let mut engine = EngineProcess::start_on(data, config, model, threads, extra, affinity)?;
    let ready = engine.ready.clone();
    println!(
        "  {} on {}, {}, {}; loaded in {:.1} s; features: {}",
        ready.model, ready.backend, ready.strategy, ready.settings, ready.load_ms / 1000.0,
        if ready.features.is_empty() { "none".to_owned() } else { ready.features.join(", ") }
    );
    let mut usage = ready.usage;
    let chunk = chunk_ms * SAMPLE_RATE as usize / 1000;
    let mut results = Vec::new();
    for clip in clips {
        while engine.events.try_recv().is_ok() {}
        let start = Instant::now();
        let mut sent = 0usize;
        for piece in clip.pcm.chunks(chunk) {
            sent += piece.len();
            if realtime {
                let due = start + Duration::from_secs_f64(sent as f64 / f64::from(SAMPLE_RATE));
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
            engine.send(&Frame::Audio(piece.to_vec()))?;
        }
        let released = Instant::now();
        engine.send(&Frame::End)?;
        let mut partials = Vec::new();
        let mut live_text = String::new();
        let (text, final_at, final_compute_ms, final_usage) = loop {
            let (at, event) = engine
                .events
                .recv_timeout(Duration::from_secs(300))
                .with_context(|| format!("no final text for {} after 300 s; {}", clip.prompt.id, engine.log.display()))?;
            match event {
                Event::Words { .. } => {}
                Event::Partial { text, input_ms, compute_ms } => {
                    if at < released {
                        live_text.clone_from(&text);
                    }
                    partials.push(PartialRecord { at_ms: ms(at - start), input_ms, compute_ms, text });
                }
                Event::Final { text, compute_ms, usage, .. } => break (text, at, compute_ms, usage),
                Event::Error { message } => bail!("{}: {message}", clip.prompt.id),
                Event::Ready(_) => {}
            }
        };
        let (errors, words) = word_errors(&clip.prompt.text, &text);
        let (live_errors, _) = word_errors(&clip.prompt.text, &live_text);
        let missed_terms: Vec<String> = clip.prompt.terms.iter().filter(|term| !has_term(term, &text)).cloned().collect();
        let lags_ms: Vec<f64> = partials.iter().map(|p| p.at_ms - p.input_ms).collect();
        let first_text_ms = partials.iter().find(|p| !p.text.trim().is_empty()).map(|p| p.at_ms - clip.onset_ms);
        let busy_ms = partials.iter().map(|p| p.compute_ms).sum::<f64>() + final_compute_ms;
        let word_delays_ms = word_delays(clip, &partials, ms(final_at - start));
        let update_ms: Vec<f64> = partials.windows(2).map(|pair| pair[1].at_ms - pair[0].at_ms).collect();
        let result = UtteranceResult {
            id: clip.prompt.id.clone(),
            audio_ms: clip.pcm.len() as f64 * 1000.0 / f64::from(SAMPLE_RATE),
            onset_ms: clip.onset_ms,
            text,
            live_text,
            errors,
            words,
            live_errors,
            terms_hit: clip.prompt.terms.len() - missed_terms.len(),
            terms: clip.prompt.terms.len(),
            missed_terms,
            final_ms: ms(final_at - released),
            final_compute_ms,
            first_text_ms,
            lags_ms,
            word_delays_ms,
            update_ms,
            busy_ms,
            cpu_ms: final_usage.cpu_ms - usage.cpu_ms,
            wall_ms: ms(final_at - start),
            partials,
        };
        usage = final_usage;
        println!(
            "  {:16} {:5.1} s  wer {:5.1}%  final {:6.0} ms  {}",
            result.id,
            result.audio_ms / 1000.0,
            percent(result.errors, result.words),
            result.final_ms,
            if result.missed_terms.is_empty() { String::new() } else { format!("missed: {}", result.missed_terms.join(", ")) }
        );
        results.push(result);
        std::thread::sleep(Duration::from_millis(200));
    }
    engine.quit();
    let summary = summarize(config, &ready, model, &results, usage);
    Ok((ready, results, summary))
}

fn summarize(config: &Config, ready: &Ready, model: &Model, results: &[UtteranceResult], usage: Usage) -> Summary {
    let sum = |f: fn(&UtteranceResult) -> usize| results.iter().map(f).sum::<usize>();
    let words = sum(|r| r.words);
    let finals: Vec<f64> = results.iter().map(|r| r.final_ms).collect();
    let lags: Vec<f64> = results.iter().flat_map(|r| r.lags_ms.iter().copied()).collect();
    let firsts: Vec<f64> = results.iter().filter_map(|r| r.first_text_ms).collect();
    let delays: Vec<f64> = results.iter().flat_map(|r| r.word_delays_ms.iter().copied()).collect();
    let updates: Vec<f64> = results.iter().flat_map(|r| r.update_ms.iter().copied()).collect();
    let audio: f64 = results.iter().map(|r| r.audio_ms).sum();
    let mb = |bytes: u64| bytes as f64 / 1_000_000.0;
    Summary {
        config: config.name.clone(),
        strategy: ready.strategy.clone(),
        settings: ready.settings.clone(),
        backend: ready.backend.clone(),
        threads: ready.threads,
        utterances: results.len(),
        wer: percent(sum(|r| r.errors), words),
        live_wer: percent(sum(|r| r.live_errors), words),
        terms: percent(sum(|r| r.terms_hit), sum(|r| r.terms)),
        final_ms_median: quantile(&finals, 0.5).unwrap_or(0.0),
        final_ms_p90: quantile(&finals, 0.9).unwrap_or(0.0),
        lag_ms_median: quantile(&lags, 0.5),
        lag_ms_p90: quantile(&lags, 0.9),
        delay_ms_median: quantile(&delays, 0.5),
        delay_ms_p90: quantile(&delays, 0.9),
        update_ms_median: quantile(&updates, 0.5),
        first_text_ms_median: quantile(&firsts, 0.5),
        busy: results.iter().map(|r| r.busy_ms).sum::<f64>() / audio,
        cores: results.iter().map(|r| r.cpu_ms).sum::<f64>() / results.iter().map(|r| r.wall_ms).sum::<f64>(),
        load_ms: ready.load_ms,
        peak_working_set_mb: mb(usage.peak_working_set),
        peak_private_mb: mb(usage.peak_private),
        model_mb: mb(model.size()),
    }
}

/// For the k-th word said: the delay from its end to when the text on screen had k words, or
/// to the final text if no live text got there. The word times come from the aligner, or else
/// from spreading the reference's words evenly over the speech.
fn word_delays(clip: &Clip, partials: &[PartialRecord], final_at_ms: f64) -> Vec<f64> {
    let ends: Vec<f64> = match &clip.word_ends_ms {
        Some(ends) => ends.clone(),
        None => {
            let words = normalize(&clip.prompt.text).len();
            let speech = (clip.end_ms - clip.onset_ms).max(1.0);
            (1..=words).map(|k| clip.onset_ms + speech * k as f64 / words as f64).collect()
        }
    };
    let counts: Vec<(f64, usize)> = partials.iter().map(|p| (p.at_ms, normalize(&p.text).len())).collect();
    ends.iter()
        .enumerate()
        .map(|(index, said)| {
            let shown = counts.iter().find(|(_, count)| *count > index).map_or(final_at_ms, |(at, _)| *at);
            (shown - said).max(0.0)
        })
        .collect()
}

/// Word end times for every clip, from one offline pass of the aligner model with word
/// timestamps, kept in the set's folder.
fn align(data: &Path, dir: &Path, models: &Models, clips: &mut [Clip]) -> Result<()> {
    const ALIGNER: &str = "tdt-v3-q8";
    let path = dir.join("word-ends.json");
    let mut known: std::collections::BTreeMap<String, Vec<f64>> =
        std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    let missing: Vec<usize> = (0..clips.len()).filter(|&i| !known.contains_key(&clips[i].prompt.id)).collect();
    if !missing.is_empty() {
        let model = models.find(ALIGNER)?;
        if !model.downloaded(data) {
            println!("No word times: the aligner {ALIGNER} is not downloaded; words are taken as evenly spread.");
            return Ok(());
        }
        println!("Aligning {} recordings with {ALIGNER}...", missing.len());
        let config = Config {
            name: "aligner".to_owned(),
            engine: "engine_transcribe_cpp".to_owned(),
            model: ALIGNER.to_owned(),
            args: ["--strategy", "offline", "--timestamps"].map(str::to_owned).to_vec(),
            final_model: None,
            vad_model: None,
        };
        let mut engine = EngineProcess::start(data, &config, model, 8, &[])?;
        for &index in &missing {
            let clip = &clips[index];
            engine.send(&Frame::Audio(clip.pcm.clone()))?;
            engine.send(&Frame::End)?;
            let mut ends = Vec::new();
            loop {
                match engine.events.recv_timeout(Duration::from_secs(120)).context("aligner")?.1 {
                    Event::Words { words } => {
                        ends = words
                            .iter()
                            .flat_map(|word| std::iter::repeat_n(word.end_ms, normalize(&word.text).len()))
                            .collect();
                    }
                    Event::Final { .. } => break,
                    Event::Error { message } => bail!("aligner: {message}"),
                    _ => {}
                }
            }
            known.insert(clip.prompt.id.clone(), ends);
        }
        engine.quit();
        std::fs::write(&path, serde_json::to_string_pretty(&known)?)?;
    }
    for clip in clips.iter_mut() {
        clip.word_ends_ms = known.get(&clip.prompt.id).cloned().filter(|ends| !ends.is_empty());
    }
    Ok(())
}

/// Scales a clip to a peak of 0.7, as the app's level control would.
fn normalize_level(pcm: &mut [f32]) {
    let peak = pcm.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    if peak > 1e-4 {
        let gain = 0.7 / peak;
        for sample in pcm.iter_mut() {
            *sample *= gain;
        }
    }
}

/// The app's level control (crates/cockpit_voice/src/level_control.rs), at 16 kHz: it follows the
/// recent peak, rising at once and halving in 3 s, and scales it to 0.7, by 10 at most. It reads
/// each sample once, in order, so applying it to a whole recording gives what the app hears live.
fn control_level(pcm: &mut [f32]) {
    const TARGET: f32 = 0.7;
    const MAX_GAIN: f32 = 10.0;
    const FLOOR: f32 = TARGET / MAX_GAIN;
    const DECAY: f32 = 0.999_985_6;
    let mut peak = FLOOR;
    for sample in pcm.iter_mut() {
        peak = (peak * DECAY).max(sample.abs()).max(FLOOR);
        *sample = (*sample * TARGET / peak).clamp(-1.0, 1.0);
    }
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 * 100.0 / whole as f64 }
}

fn quantile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted.get(index).copied()
}

fn print_summary(s: &Summary) {
    println!(
        "  => wer {:.1}% (live {:.1}%), terms {:.0}%, final {:.0} ms (p90 {:.0}), text delay {} (p90 {}), updates every {}, compute lag {}, first text {}, busy {:.0}%, {:.1} cores, peak {:.0} MB ({:.0} MB private)",
        s.wer, s.live_wer, s.terms, s.final_ms_median, s.final_ms_p90,
        show_ms(s.delay_ms_median), show_ms(s.delay_ms_p90), show_ms(s.update_ms_median),
        show_ms(s.lag_ms_median), show_ms(s.first_text_ms_median),
        s.busy * 100.0, s.cores, s.peak_working_set_mb, s.peak_private_mb
    );
}

fn show_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{v:.0} ms"))
}

fn table(summaries: &[Summary]) -> String {
    let mut out = String::from(
        "| Config | Live text | WER | Live WER | Terms | Final after release | Text delay | Updates every | First text | Busy | Cores | Peak RAM (private) | Load | Model |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for s in summaries {
        out.push_str(&format!(
            "| {} | {} | {:.1}% | {:.1}% | {:.0}% | {:.0} ms (p90 {:.0}) | {} (p90 {}) | {} | {} | {:.0}% | {:.1} | {:.0} MB ({:.0}) | {:.1} s | {:.0} MB |\n",
            s.config, s.strategy, s.wer, s.live_wer, s.terms, s.final_ms_median, s.final_ms_p90,
            show_ms(s.delay_ms_median), show_ms(s.delay_ms_p90), show_ms(s.update_ms_median), show_ms(s.first_text_ms_median),
            s.busy * 100.0, s.cores, s.peak_working_set_mb, s.peak_private_mb, s.load_ms / 1000.0, s.model_mb
        ));
    }
    out
}

