//! Times candle's matrix products at the sizes of Parakeet's encoder, with 8-bit (Q8_0) and f32
//! weights. The thread count comes from RAYON_NUM_THREADS. kernels/ggml/bench.c times the same.

use std::time::Instant;

use anyhow::Result;
use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
use candle_core::{Device, Module, Tensor};

/// (output features, input features): the feed-forward up and down projections, and attention.
const WEIGHTS: [(usize, usize); 3] = [(4096, 1024), (1024, 4096), (1024, 1024)];
/// Frames of 80 ms: one 560 ms chunk, 5 s, 10 s and 30 s of speech.
const FRAMES: [usize; 4] = [7, 64, 125, 375];

fn main() -> Result<()> {
    let device = Device::Cpu;
    let threads = std::env::var("RAYON_NUM_THREADS").unwrap_or_default();
    for (n, k) in WEIGHTS {
        let weight = Tensor::randn(0f32, 1.0, (n, k), &device)?;
        let q8 = QMatMul::from_qtensor(QTensor::quantize(&weight, GgmlDType::Q8_0)?)?;
        let f32 = QMatMul::Tensor(weight.clone());
        for m in FRAMES {
            let input = Tensor::randn(0f32, 1.0, (m, k), &device)?;
            for (kind, product) in [("q8_0", &q8), ("f32", &f32)] {
                let ms = median_ms(|| {
                    let _ = product.forward(&input)?;
                    Ok(())
                })?;
                let gflops = 2.0 * (m * n * k) as f64 / (ms * 1e6);
                println!(
                    "{}",
                    serde_json::json!({"lib": "candle", "type": kind, "m": m, "n": n, "k": k,
                        "threads": threads, "ms": ms, "gflops": gflops})
                );
            }
        }
    }
    Ok(())
}

/// The median of runs until 0.5 s have passed, after two warm-up runs.
fn median_ms(mut run: impl FnMut() -> Result<()>) -> Result<f64> {
    run()?;
    run()?;
    let mut times = Vec::new();
    let start = Instant::now();
    while times.len() < 5 || (start.elapsed().as_secs_f64() < 0.5 && times.len() < 2000) {
        let begin = Instant::now();
        run()?;
        times.push(begin.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    Ok(times[times.len() / 2])
}
