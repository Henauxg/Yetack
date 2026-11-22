/// A model file the app downloads once. Each is pinned to a commit of its Hugging Face
/// repository, and checked by its SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// The file's name in the models folder.
    pub name: &'static str,
    pub url: &'static str,
    pub size: u64,
    /// In lowercase hex.
    pub sha256: &'static str,
}

/// Parakeet Ultra, by Moondream after NVIDIA's Parakeet TDT 0.6B v3, in 8 bits for ggml. CC-BY-4.0.
pub(crate) const PARAKEET: ModelFile = ModelFile {
    name: "parakeet-ultra-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-ultra-gguf/resolve/a1539be1b849ea7463b88343e5277b373d0c4ea9/parakeet-ultra-Q8_0.gguf",
    size: 740_363_168,
    sha256: "007a59761e9258779f189df396b50d12b83bb5b79e66df4b955c230d2f2a0a59",
};

/// Silero VAD v6.2, which tells speech from other sounds. MIT.
pub(crate) const SILERO: ModelFile = ModelFile {
    name: "silero-vad-v6.2-F32.gguf",
    url: "https://huggingface.co/handy-computer/silero-vad-v6.2-gguf/resolve/a9b2d61c20ae4ddf3644edb34969e8185a485448/silero-vad-v6.2-F32.gguf",
    size: 1_241_056,
    sha256: "0d871a800c420fdd5d1b17a3aea4296d1c39608f0ff4a9295eb76457eb1c88e6",
};

/// Every file dictation needs, in the models folder.
pub const MODEL_FILES: [ModelFile; 2] = [PARAKEET, SILERO];
