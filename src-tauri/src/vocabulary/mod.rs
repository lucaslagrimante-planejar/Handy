//! Fork feature: custom words learned per transcription model from the
//! dictation history. See [`learn`] for how they are found and [`store`] for
//! where they live.

pub mod learn;
pub mod store;

pub use store::custom_words_for;
