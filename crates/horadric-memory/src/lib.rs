//! Memory: text found by what it means, not only by the words it shares
//! with the question. See docs/plan/memory.md.
//!
//! This part turns text into vectors. The model runs here, in Rust, with
//! candle: nothing to install beside Horadric and no service that has to
//! be running for recall to work. [`store`] keeps the text and the
//! vectors on disk, [`recall`] ranks what answers a question, and
//! [`keeper::Keeper`] is all of it as the app runs it.

mod gemma;
pub mod horadrim;
pub mod keeper;
pub mod models;
pub mod recall;
pub mod store;

use std::path::Path;

use candle_core::{DType, Device, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

/// The models Horadric can embed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// google/embeddinggemma-300m: 768 numbers, multilingual, cut to 256
    /// or 128 by keeping the first ones (it was trained for that).
    EmbeddingGemma,
    /// BAAI/bge-small-en-v1.5: 384 numbers, English, pooled on the first
    /// token.
    BgeSmall,
    /// sentence-transformers/all-MiniLM-L6-v2: 384 numbers, English,
    /// mean pooled, six layers.
    MiniLm,
}

impl Kind {
    /// The model's folder name, under the state folder's `models\`.
    pub fn folder(self) -> &'static str {
        match self {
            Kind::EmbeddingGemma => "embeddinggemma-300m",
            Kind::BgeSmall => "bge-small-en-v1.5",
            Kind::MiniLm => "all-MiniLM-L6-v2",
        }
    }

    /// The name a vector made by this model is kept under: the model and
    /// the weights' commit where one is pinned. A new one means the kept
    /// text is embedded again.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::EmbeddingGemma => "embeddinggemma-300m",
            Kind::BgeSmall => "bge-small-en-v1.5",
            Kind::MiniLm => "all-MiniLM-L6-v2@1110a243",
        }
    }

    /// What a question is prefixed with: the models were trained with
    /// these, and recall drops without them.
    fn query_prompt(self) -> &'static str {
        match self {
            Kind::EmbeddingGemma => "task: search result | query: ",
            Kind::BgeSmall => "Represent this sentence for searching relevant passages: ",
            Kind::MiniLm => "",
        }
    }

    /// The longest input read, in tokens: what each model was trained to
    /// read, as sentence-transformers cuts it.
    pub fn most_tokens(self) -> usize {
        match self {
            Kind::EmbeddingGemma => gemma::MOST_TOKENS,
            Kind::BgeSmall => 512,
            Kind::MiniLm => 256,
        }
    }

    fn document_prompt(self) -> &'static str {
        match self {
            Kind::EmbeddingGemma => "title: none | text: ",
            Kind::BgeSmall | Kind::MiniLm => "",
        }
    }
}

enum Net {
    Gemma(gemma::Gemma),
    Bert { model: BertModel, mean: bool },
}

pub struct Embedder {
    kind: Kind,
    net: Net,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Loads `kind` from `dir`, the folder its files were fetched into.
    pub fn load(kind: Kind, dir: &Path) -> Result<Embedder, String> {
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| e.to_string())?;
        // Some tokenizers come set to pad every input to one length; one
        // input at a time needs none, and pads would be pooled as words.
        tokenizer.with_padding(None);
        tokenizer.with_truncation(None).map_err(|e| e.to_string())?;
        let err = |e: candle_core::Error| e.to_string();
        let net = match kind {
            Kind::EmbeddingGemma => Net::Gemma(gemma::Gemma::load(dir, true).map_err(err)?),
            Kind::BgeSmall | Kind::MiniLm => {
                let cfg: BertConfig = serde_json::from_slice(
                    &std::fs::read(dir.join("config.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                // SAFETY: the file is only read, and nothing writes it
                // while a model is loaded.
                let vb = unsafe {
                    VarBuilder::from_mmaped_safetensors(
                        &[dir.join("model.safetensors")],
                        DType::F32,
                        &Device::Cpu,
                    )
                    .map_err(err)?
                };
                Net::Bert {
                    model: BertModel::load(vb, &cfg).map_err(err)?,
                    mean: kind == Kind::MiniLm,
                }
            }
        };
        Ok(Embedder {
            kind,
            net,
            tokenizer,
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The vector for a question.
    pub fn query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.embed(&format!("{}{text}", self.kind.query_prompt()))
    }

    /// The vector for a passage that may answer one.
    pub fn document(&self, text: &str) -> Result<Vec<f32>, String> {
        self.embed(&format!("{}{text}", self.kind.document_prompt()))
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| e.to_string())?;
        let ids = keep_ends(enc.get_ids(), self.kind.most_tokens());
        let err = |e: candle_core::Error| e.to_string();
        match &self.net {
            Net::Gemma(g) => g.embed(&ids).map_err(err),
            Net::Bert { model, mean } => bert(model, &ids, *mean).map_err(err),
        }
    }
}

fn bert(model: &BertModel, ids: &[u32], mean: bool) -> candle_core::Result<Vec<f32>> {
    let dev = Device::Cpu;
    let input = Tensor::new(ids, &dev)?.unsqueeze(0)?;
    let types = input.zeros_like()?;
    let out = model.forward(&input, &types, None)?;
    let pooled = if mean {
        out.mean(1)?
    } else {
        out.narrow(1, 0, 1)?.squeeze(1)?
    };
    let unit = pooled.broadcast_div(&pooled.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
    unit.squeeze(0)?.to_vec1()
}

/// The first `most` ids, keeping the last one (the end token the models
/// were trained to see) when the input is cut.
fn keep_ends(ids: &[u32], most: usize) -> Vec<u32> {
    if ids.len() <= most {
        return ids.to_vec();
    }
    let mut out = ids[..most - 1].to_vec();
    out.extend(ids.last());
    out
}

/// The first `dims` numbers of `v`, made a unit vector again. Matryoshka
/// models put the most meaning first, so this is how they are made small.
pub fn truncate(v: &[f32], dims: usize) -> Vec<f32> {
    let v = &v[..dims.min(v.len())];
    let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if len == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / len).collect()
}

/// The cosine of two unit vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_input_keeps_its_end_token() {
        assert_eq!(keep_ends(&[2, 5, 6, 7, 1], 3), vec![2, 5, 1]);
        assert_eq!(keep_ends(&[2, 5, 1], 3), vec![2, 5, 1]);
    }

    #[test]
    fn truncating_keeps_the_start_and_unit_length() {
        let t = truncate(&[3.0, 4.0, 12.0], 2);
        assert!((t[0] - 0.6).abs() < 1e-6 && (t[1] - 0.8).abs() < 1e-6);
        assert!((similarity(&t, &t) - 1.0).abs() < 1e-6);
    }
}
