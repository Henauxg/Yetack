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

/// The model that writes the text. Both are Parakeet models of 0.6 billion weights, in 8 bits for
/// ggml, and cost the same to run (docs/voice/investigation.md, "English-only models").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpeechModel {
    /// Parakeet unified EN, by NVIDIA: English alone, so it never switches to another language.
    /// NVIDIA Open Model License.
    #[default]
    UnifiedEn,
    /// Parakeet Ultra, by Moondream after NVIDIA's Parakeet TDT 0.6B v3: 25 European languages,
    /// picked for each segment. CC-BY-4.0.
    Ultra,
}

impl SpeechModel {
    pub const ALL: [Self; 2] = [Self::UnifiedEn, Self::Ultra];

    /// The model's own file.
    pub const fn file(self) -> ModelFile {
        match self {
            Self::UnifiedEn => UNIFIED_EN,
            Self::Ultra => ULTRA,
        }
    }

    /// Every file dictation needs with this model, in the models folder: the model's own, and
    /// Silero's, which every model shares.
    pub const fn files(self) -> [ModelFile; 2] {
        [self.file(), SILERO]
    }
}

const UNIFIED_EN: ModelFile = ModelFile {
    name: "parakeet-unified-en-0.6b-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q8_0.gguf",
    size: 731_357_568,
    sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
};

const ULTRA: ModelFile = ModelFile {
    name: "parakeet-ultra-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-ultra-gguf/resolve/a1539be1b849ea7463b88343e5277b373d0c4ea9/parakeet-ultra-Q8_0.gguf",
    size: 740_363_168,
    sha256: "007a59761e9258779f189df396b50d12b83bb5b79e66df4b955c230d2f2a0a59",
};

/// Silero VAD v6.2, which tells speech from other sounds. MIT.
pub const SILERO: ModelFile = ModelFile {
    name: "silero-vad-v6.2-F32.gguf",
    url: "https://huggingface.co/handy-computer/silero-vad-v6.2-gguf/resolve/a9b2d61c20ae4ddf3644edb34969e8185a485448/silero-vad-v6.2-F32.gguf",
    size: 1_241_056,
    sha256: "0d871a800c420fdd5d1b17a3aea4296d1c39608f0ff4a9295eb76457eb1c88e6",
};
