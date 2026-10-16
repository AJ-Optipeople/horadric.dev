//! EmbeddingGemma: Gemma 3's text layers read both ways, mean pooled,
//! then two dense layers. candle's own Gemma 3 is a causal decoder with
//! a key value cache, built to write text; an embedder needs every token
//! to see every other, so the layers are written again here, smaller.

use std::path::Path;

use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::{linear_no_bias, Activation, Embedding, Linear, VarBuilder};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub head_dim: usize,
    pub hidden_activation: Activation,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_attention_heads: usize,
    pub num_hidden_layers: usize,
    pub num_key_value_heads: usize,
    pub rms_norm_eps: f64,
    pub rope_theta: f64,
    pub rope_local_base_freq: f64,
    pub vocab_size: usize,
    pub query_pre_attn_scalar: usize,
    pub sliding_window: usize,
    pub layer_types: Vec<String>,
}

/// The longest input read, in tokens. Memory chunks are far shorter.
pub const MOST_TOKENS: usize = 512;

struct RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl RmsNorm {
    fn new(dim: usize, eps: f64, vb: VarBuilder) -> Result<Self> {
        // Gemma keeps the weight as an offset from one.
        let weight = (vb.get(dim, "weight")? + 1.0)?;
        Ok(Self { weight, eps })
    }
}

impl Module for RmsNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        candle_nn::ops::rms_norm(&x.contiguous()?, &self.weight, self.eps as f32)
    }
}

struct Rope {
    sin: Tensor,
    cos: Tensor,
}

impl Rope {
    fn new(dim: usize, base: f64, dev: &Device) -> Result<Self> {
        let inv: Vec<f32> = (0..dim)
            .step_by(2)
            .map(|i| 1.0 / base.powf(i as f64 / dim as f64) as f32)
            .collect();
        let n = inv.len();
        let inv = Tensor::from_vec(inv, (1, n), dev)?;
        let t = Tensor::arange(0u32, MOST_TOKENS as u32, dev)?
            .to_dtype(DType::F32)?
            .reshape((MOST_TOKENS, 1))?;
        let freqs = t.matmul(&inv)?;
        Ok(Self {
            sin: freqs.sin()?,
            cos: freqs.cos()?,
        })
    }

    fn apply(&self, x: &Tensor) -> Result<Tensor> {
        let len = x.dim(2)?;
        candle_nn::rotary_emb::rope(
            &x.contiguous()?,
            &self.cos.narrow(0, 0, len)?,
            &self.sin.narrow(0, 0, len)?,
        )
    }
}

struct Layer {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    gate: Linear,
    up: Linear,
    down: Linear,
    act: Activation,
    input_norm: RmsNorm,
    post_attn_norm: RmsNorm,
    pre_ff_norm: RmsNorm,
    post_ff_norm: RmsNorm,
    /// Index into [`Gemma::ropes`]: sliding layers turn slower than full ones.
    rope: usize,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
    scale: f64,
}

impl Layer {
    fn new(cfg: &Config, rope: usize, vb: VarBuilder) -> Result<Self> {
        let (h, hd, eps) = (cfg.hidden_size, cfg.head_dim, cfg.rms_norm_eps);
        let a = vb.pp("self_attn");
        let m = vb.pp("mlp");
        Ok(Self {
            q: linear_no_bias(h, cfg.num_attention_heads * hd, a.pp("q_proj"))?,
            k: linear_no_bias(h, cfg.num_key_value_heads * hd, a.pp("k_proj"))?,
            v: linear_no_bias(h, cfg.num_key_value_heads * hd, a.pp("v_proj"))?,
            o: linear_no_bias(cfg.num_attention_heads * hd, h, a.pp("o_proj"))?,
            q_norm: RmsNorm::new(hd, eps, a.pp("q_norm"))?,
            k_norm: RmsNorm::new(hd, eps, a.pp("k_norm"))?,
            gate: linear_no_bias(h, cfg.intermediate_size, m.pp("gate_proj"))?,
            up: linear_no_bias(h, cfg.intermediate_size, m.pp("up_proj"))?,
            down: linear_no_bias(cfg.intermediate_size, h, m.pp("down_proj"))?,
            act: cfg.hidden_activation,
            input_norm: RmsNorm::new(h, eps, vb.pp("input_layernorm"))?,
            post_attn_norm: RmsNorm::new(h, eps, vb.pp("post_attention_layernorm"))?,
            pre_ff_norm: RmsNorm::new(h, eps, vb.pp("pre_feedforward_layernorm"))?,
            post_ff_norm: RmsNorm::new(h, eps, vb.pp("post_feedforward_layernorm"))?,
            rope,
            heads: cfg.num_attention_heads,
            kv_heads: cfg.num_key_value_heads,
            head_dim: hd,
            scale: 1.0 / (cfg.query_pre_attn_scalar as f64).sqrt(),
        })
    }

    fn forward(&self, xs: &Tensor, rope: &Rope, mask: Option<&Tensor>) -> Result<Tensor> {
        let (b, len, _) = xs.dims3()?;
        let x = self.input_norm.forward(xs)?;
        let split = |t: Tensor, n: usize| {
            t.reshape((b, len, n, self.head_dim))?
                .transpose(1, 2)?
                .contiguous()
        };
        let q = split(self.q.forward(&x)?, self.heads)?;
        let k = split(self.k.forward(&x)?, self.kv_heads)?;
        let v = split(self.v.forward(&x)?, self.kv_heads)?;
        let q = rope.apply(&self.q_norm.forward(&q)?)?;
        let k = rope.apply(&self.k_norm.forward(&k)?)?;
        let groups = self.heads / self.kv_heads;
        let k = candle_transformers::utils::repeat_kv(k, groups)?.contiguous()?;
        let v = candle_transformers::utils::repeat_kv(v, groups)?.contiguous()?;
        let w = (q.matmul(&k.transpose(2, 3)?)? * self.scale)?;
        let w = match mask {
            Some(m) => w.broadcast_add(m)?,
            None => w,
        };
        let w = candle_nn::ops::softmax_last_dim(&w)?;
        let attn = w
            .matmul(&v)?
            .transpose(1, 2)?
            .reshape((b, len, ()))?
            .apply(&self.o)?;
        let xs = (xs + self.post_attn_norm.forward(&attn)?)?;
        let x = self.pre_ff_norm.forward(&xs)?;
        let ff =
            (self.gate.forward(&x)?.apply(&self.act)? * self.up.forward(&x)?)?.apply(&self.down)?;
        xs + self.post_ff_norm.forward(&ff)?
    }
}

pub struct Gemma {
    embed: Embedding,
    layers: Vec<Layer>,
    ropes: [Rope; 2],
    norm: RmsNorm,
    dense: [Linear; 2],
    hidden: usize,
    /// How far apart two tokens may be and still see each other in a
    /// sliding layer.
    window: usize,
}

impl Gemma {
    /// Loads the model laid out as sentence-transformers saves it: the
    /// text model at the top of `dir`, the two dense layers in
    /// `2_Dense` and `3_Dense`. The token table, a fifth of a billion
    /// numbers, is kept in half precision when `small_table` is set,
    /// which halves the memory the model takes.
    pub fn load(dir: &Path, small_table: bool) -> Result<Gemma> {
        let dev = Device::Cpu;
        let cfg: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)
            .map_err(candle_core::Error::wrap)?;
        // SAFETY: the files are only read, and Horadric never writes them
        // while a model is loaded.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &dev)?
        };
        let table = vb
            .pp("embed_tokens")
            .get((cfg.vocab_size, cfg.hidden_size), "weight")?;
        let table = if small_table {
            table.to_dtype(DType::F16)?
        } else {
            table
        };
        let embed = Embedding::new(table, cfg.hidden_size);
        let layers = (0..cfg.num_hidden_layers)
            .map(|i| {
                let full = cfg
                    .layer_types
                    .get(i)
                    .is_some_and(|t| t == "full_attention");
                Layer::new(&cfg, usize::from(full), vb.pp("layers").pp(i))
            })
            .collect::<Result<Vec<_>>>()?;
        let ropes = [
            Rope::new(cfg.head_dim, cfg.rope_local_base_freq, &dev)?,
            Rope::new(cfg.head_dim, cfg.rope_theta, &dev)?,
        ];
        let norm = RmsNorm::new(cfg.hidden_size, cfg.rms_norm_eps, vb.pp("norm"))?;
        let dense = |sub: &str| -> Result<Linear> {
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(
                    &[dir.join(sub).join("model.safetensors")],
                    DType::F32,
                    &dev,
                )?
            };
            let w = vb.pp("linear").get_unchecked("weight")?;
            Ok(Linear::new(w, None))
        };
        Ok(Gemma {
            embed,
            layers,
            ropes,
            norm,
            dense: [dense("2_Dense")?, dense("3_Dense")?],
            hidden: cfg.hidden_size,
            // Read both ways, Hugging Face halves the window to each side,
            // plus one: the reference implementation, and so the training.
            window: cfg.sliding_window / 2 + 1,
        })
    }

    /// The unit vector for one input's token ids, at most [`MOST_TOKENS`].
    pub fn embed(&self, ids: &[u32]) -> Result<Vec<f32>> {
        let dev = Device::Cpu;
        let len = ids.len();
        let ids = Tensor::new(ids, &dev)?.unsqueeze(0)?;
        let mask = if len > self.window {
            Some(Tensor::from_vec(band(len, self.window), (len, len), &dev)?)
        } else {
            None
        };
        let mut xs =
            (self.embed.forward(&ids)?.to_dtype(DType::F32)? * (self.hidden as f64).sqrt())?;
        for layer in &self.layers {
            let sliding = mask.as_ref().filter(|_| layer.rope == 0);
            xs = layer.forward(&xs, &self.ropes[layer.rope], sliding)?;
        }
        let pooled = self.norm.forward(&xs)?.mean(1)?;
        let out = pooled.apply(&self.dense[0])?.apply(&self.dense[1])?;
        let out = out.broadcast_div(&out.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
        out.squeeze(0)?.to_vec1()
    }
}

/// The mask of a sliding layer read both ways: token `i` sees token `j`
/// when they are fewer than `window` apart.
fn band(len: usize, window: usize) -> Vec<f32> {
    (0..len * len)
        .map(|n| {
            let (i, j) = (n / len, n % len);
            if i.abs_diff(j) < window {
                0.0
            } else {
                f32::NEG_INFINITY
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sliding_layer_sees_only_its_neighbours() {
        let inf = f32::NEG_INFINITY;
        assert_eq!(
            band(3, 2),
            vec![0.0, 0.0, inf, 0.0, 0.0, 0.0, inf, 0.0, 0.0]
        );
    }
}
