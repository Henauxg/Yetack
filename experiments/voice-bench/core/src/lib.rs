//! What the bench's driver and its engines share: the pipe protocol, the engine loop, audio
//! files, resampling, process usage and transcript scoring.

pub mod args;
pub mod audio;
pub mod engine;
pub mod protocol;
pub mod score;
pub mod usage;

use std::time::Duration;

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
