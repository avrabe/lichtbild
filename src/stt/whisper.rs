//! Whisper decoding, adapted from candle-examples 0.11.1,
//! examples/whisper/main.rs (https://github.com/huggingface/candle, MIT OR
//! Apache-2.0). lichtbild's changes: the encoder runs once per window and
//! its output is shared by language detection and decoding; the language is
//! chosen per window; the compression ratio is computed, so the temperature
//! fallback catches repetition loops; no printing.

use anyhow::{Error as E, Result};
use candle::{Device, IndexOp, Tensor};
use candle_nn::ops::{log_softmax, softmax};
use rand::distr::weighted::WeightedIndex;
use rand::distr::Distribution;
use rand::SeedableRng;
use tokenizers::Tokenizer;

use candle_transformers::models::whisper::{self as m, Config};

pub enum Model {
    Normal(m::model::Whisper),
    Quantized(m::quantized_model::Whisper),
}

// Maybe we should use some traits rather than doing the dispatch for all these.
impl Model {
    pub fn config(&self) -> &Config {
        match self {
            Self::Normal(m) => &m.config,
            Self::Quantized(m) => &m.config,
        }
    }

    pub fn encoder_forward(&mut self, x: &Tensor, flush: bool) -> candle::Result<Tensor> {
        match self {
            Self::Normal(m) => m.encoder.forward(x, flush),
            Self::Quantized(m) => m.encoder.forward(x, flush),
        }
    }

    pub fn decoder_forward(
        &mut self,
        x: &Tensor,
        xa: &Tensor,
        flush: bool,
    ) -> candle::Result<Tensor> {
        match self {
            Self::Normal(m) => m.decoder.forward(x, xa, flush),
            Self::Quantized(m) => m.decoder.forward(x, xa, flush),
        }
    }

    pub fn decoder_final_linear(&self, x: &Tensor) -> candle::Result<Tensor> {
        match self {
            Self::Normal(m) => m.decoder.final_linear(x),
            Self::Quantized(m) => m.decoder.final_linear(x),
        }
    }
}

/// Start and end (seconds from the window start) and text.
pub type Timed = (f32, f32, String);

/// `text` and `temperature` are kept for debugging output.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DecodingResult {
    pub tokens: Vec<u32>,
    pub text: String,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub temperature: f64,
    pub compression_ratio: f64,
}

pub struct Decoder {
    model: Model,
    rng: rand::rngs::StdRng,
    timestamps: bool,
    max_initial_timestamp_index: Option<u32>,
    tokenizer: Tokenizer,
    suppress_tokens: Tensor,
    sot_token: u32,
    transcribe_token: u32,
    eot_token: u32,
    no_speech_token: u32,
    no_timestamps_token: u32,
    language_token: Option<u32>,
    all_languages: Vec<u32>,
}

impl Decoder {
    pub fn new(
        model: Model,
        tokenizer: Tokenizer,
        seed: u64,
        device: &Device,
        language_token: Option<u32>,
        timestamps: bool,
        max_initial_timestamp_index: Option<u32>,
    ) -> Result<Self> {
        let no_timestamps_token = token_id(&tokenizer, m::NO_TIMESTAMPS_TOKEN)?;
        // Suppress the notimestamps token when in timestamps mode.
        // https://github.com/openai/whisper/blob/e8622f9afc4eba139bf796c210f5c01081000472/whisper/decoding.py#L452
        let suppress_tokens: Vec<f32> = (0..model.config().vocab_size as u32)
            .map(|i| {
                if model.config().suppress_tokens.contains(&i)
                    || timestamps && i == no_timestamps_token
                {
                    f32::NEG_INFINITY
                } else {
                    0f32
                }
            })
            .collect();
        let suppress_tokens = Tensor::new(suppress_tokens.as_slice(), device)?;
        let sot_token = token_id(&tokenizer, m::SOT_TOKEN)?;
        let transcribe_token = token_id(&tokenizer, m::TRANSCRIBE_TOKEN)?;
        let eot_token = token_id(&tokenizer, m::EOT_TOKEN)?;
        let no_speech_token = m::NO_SPEECH_TOKENS
            .iter()
            .find_map(|token| token_id(&tokenizer, token).ok());
        let no_speech_token = match no_speech_token {
            None => anyhow::bail!("unable to find any non-speech token"),
            Some(n) => n,
        };
        let all_languages = crate::stt::LANGUAGES
            .iter()
            .filter_map(|(code, _)| token_id(&tokenizer, &format!("<|{code}|>")).ok())
            .collect();
        Ok(Self {
            model,
            rng: rand::rngs::StdRng::seed_from_u64(seed),
            tokenizer,
            timestamps,
            max_initial_timestamp_index,
            suppress_tokens,
            sot_token,
            transcribe_token,
            eot_token,
            no_speech_token,
            language_token,
            no_timestamps_token,
            all_languages,
        })
    }

    /// The encoder output for one window of at most 3000 mel frames.
    pub fn features(&mut self, mel: &Tensor) -> Result<Tensor> {
        Ok(self.model.encoder_forward(mel, true)?)
    }

    /// The most likely language token for a window, among `allowed` (all
    /// of Whisper's languages when empty).
    pub fn language(&mut self, features: &Tensor, allowed: &[u32]) -> Result<u32> {
        let device = features.device().clone();
        let tokens = Tensor::new(&[[self.sot_token]], &device)?;
        let ys = self.model.decoder_forward(&tokens, features, true)?;
        let logits = self.model.decoder_final_linear(&ys.i(..1)?)?.i(0)?.i(0)?;
        let candidates: Vec<u32> = if allowed.is_empty() {
            self.all_languages.clone()
        } else {
            allowed.to_vec()
        };
        let ids = Tensor::new(candidates.as_slice(), &device)?;
        let scores: Vec<f32> = logits.index_select(&ids, 0)?.to_vec1()?;
        let best = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| candidates[i])
            .ok_or_else(|| anyhow::anyhow!("no language candidates"))?;
        Ok(best)
    }

    fn decode(&mut self, audio_features: &Tensor, language: u32, t: f64) -> Result<DecodingResult> {
        self.language_token = Some(language);
        let sample_len = self.model.config().max_target_positions / 2;
        let mut sum_logprob = 0f64;
        let mut no_speech_prob = f64::NAN;
        let mut tokens = vec![self.sot_token];
        if let Some(language_token) = self.language_token {
            tokens.push(language_token);
        }
        tokens.push(self.transcribe_token);
        if !self.timestamps {
            tokens.push(self.no_timestamps_token);
        }
        for i in 0..sample_len {
            let tokens_t = Tensor::new(tokens.as_slice(), audio_features.device())?;

            // The model expects a batch dim but this inference loop does not handle
            // it so we add it at this point.
            let tokens_t = tokens_t.unsqueeze(0)?;
            let ys = self
                .model
                .decoder_forward(&tokens_t, audio_features, i == 0)?;

            // Extract the no speech probability on the first iteration by looking at the first
            // token logits and the probability for the according token.
            if i == 0 {
                let logits = self.model.decoder_final_linear(&ys.i(..1)?)?.i(0)?.i(0)?;
                no_speech_prob = softmax(&logits, 0)?
                    .i(self.no_speech_token as usize)?
                    .to_scalar::<f32>()? as f64;
            }

            let (_, seq_len, _) = ys.dims3()?;
            let logits = self
                .model
                .decoder_final_linear(&ys.i((..1, seq_len - 1..))?)?
                .i(0)?
                .i(0)?;

            // Apply timestamp rules when timestamps are enabled
            let logits = if self.timestamps {
                self.apply_timestamp_rules(&logits, &tokens)?
            } else {
                logits
            };

            let logits = logits.broadcast_add(&self.suppress_tokens)?;
            let next_token = if t > 0f64 {
                let prs = softmax(&(&logits / t)?, 0)?;
                let logits_v: Vec<f32> = prs.to_vec1()?;
                let distr = WeightedIndex::new(&logits_v)?;
                distr.sample(&mut self.rng) as u32
            } else {
                let logits_v: Vec<f32> = logits.to_vec1()?;
                logits_v
                    .iter()
                    .enumerate()
                    .max_by(|(_, u), (_, v)| u.total_cmp(v))
                    .map(|(i, _)| i as u32)
                    .unwrap()
            };
            tokens.push(next_token);
            let prob = softmax(&logits, candle::D::Minus1)?
                .i(next_token as usize)?
                .to_scalar::<f32>()? as f64;
            if next_token == self.eot_token
                || tokens.len() > self.model.config().max_target_positions
            {
                break;
            }
            sum_logprob += prob.ln();
        }
        let text = self.tokenizer.decode(&tokens, true).map_err(E::msg)?;
        let avg_logprob = sum_logprob / tokens.len() as f64;
        let compression_ratio = compression_ratio(&text);

        Ok(DecodingResult {
            tokens,
            text,
            avg_logprob,
            no_speech_prob,
            temperature: t,
            compression_ratio,
        })
    }

    /// Split a decoded window into timed pieces (seconds from the
    /// window start). Also returns where the next window should start: the
    /// last timestamp, or `None` (no timestamp at all) to take the whole
    /// window.
    pub fn timed_text(&self, tokens: &[u32]) -> Result<(Vec<Timed>, Option<f32>)> {
        let ts = |t: u32| (t - self.no_timestamps_token - 1) as f32 * 0.02;
        let mut out = Vec::new();
        let mut text_tokens = Vec::new();
        let mut start: Option<f32> = None;
        for &t in tokens {
            if t == self.sot_token
                || t == self.eot_token
                || (t >= self.sot_token && t <= self.no_timestamps_token)
            {
                continue;
            }
            if t > self.no_timestamps_token {
                let at = ts(t);
                if !text_tokens.is_empty() {
                    let text = self.tokenizer.decode(&text_tokens, true).map_err(E::msg)?;
                    out.push((start.unwrap_or(0.0), at, text.trim().to_string()));
                    text_tokens.clear();
                }
                start = Some(at);
            } else {
                text_tokens.push(t);
            }
        }
        // Text after the last timestamp ran past the window: it is dropped
        // and decoded again from its start in the next window, so the word
        // cut at the seam comes out whole. Otherwise the next window starts
        // where the last text ended: Whisper often stops early, and the
        // rest of the window must not be skipped.
        let resume = match (text_tokens.is_empty(), start) {
            (_, Some(at)) => Some(at),
            (false, None) => {
                let text = self.tokenizer.decode(&text_tokens, true).map_err(E::msg)?;
                out.push((0.0, 30.0, text.trim().to_string()));
                None
            }
            (true, None) => None,
        };
        Ok((
            out.into_iter().filter(|p| !p.2.is_empty()).collect(),
            resume,
        ))
    }

    /// Decode a window, raising the temperature while the result looks
    /// like a loop (compression ratio) or is unlikely (log probability).
    pub fn decode_with_fallback(
        &mut self,
        features: &Tensor,
        language: u32,
    ) -> Result<DecodingResult> {
        for (i, &t) in m::TEMPERATURES.iter().enumerate() {
            let dr: Result<DecodingResult> = self.decode(features, language, t);
            if i == m::TEMPERATURES.len() - 1 {
                return dr;
            }
            // On errors, we try again with a different temperature.
            if let Ok(dr) = dr {
                let needs_fallback = dr.compression_ratio > m::COMPRESSION_RATIO_THRESHOLD
                    || dr.avg_logprob < m::LOGPROB_THRESHOLD;
                if !needs_fallback || dr.no_speech_prob > m::NO_SPEECH_THRESHOLD {
                    return Ok(dr);
                }
            }
        }
        unreachable!()
    }

    fn apply_timestamp_rules(&self, input_logits: &Tensor, tokens: &[u32]) -> Result<Tensor> {
        let device = input_logits.device().clone();
        let timestamp_begin = self.no_timestamps_token + 1;
        let vocab_size = self.model.config().vocab_size as u32;

        // ========== SETUP: Extract sampled tokens for analysis ==========
        let sample_begin = if self.language_token.is_some() { 3 } else { 2 };
        let sampled_tokens = if tokens.len() > sample_begin {
            &tokens[sample_begin..]
        } else {
            &[]
        };

        let mut masks = Vec::new();
        // Pre-allocate reusable mask buffer to avoid repeated allocations
        let mut mask_buffer = vec![0.0f32; vocab_size as usize];

        // ========== RULE 1: Timestamp pairing constraints ==========
        // Timestamps must come in pairs, except directly before EOT
        if !sampled_tokens.is_empty() {
            let last_was_timestamp = sampled_tokens
                .last()
                .map(|&t| t >= timestamp_begin)
                .unwrap_or(false);

            // As in OpenAI's ApplyTimestampRules: with fewer than two
            // sampled tokens the "penultimate" counts as a timestamp, so the
            // window's first timestamp must be followed by text. candle's
            // example has `false` here, which forces a second timestamp
            // right away and lets a window end as "<0.00><29.90>" with no
            // text, skipping 30 s of speech (measured on open media).
            let penultimate_was_timestamp = if sampled_tokens.len() >= 2 {
                sampled_tokens[sampled_tokens.len() - 2] >= timestamp_begin
            } else {
                true
            };

            if last_was_timestamp {
                if penultimate_was_timestamp {
                    // Has to be non-timestamp - suppress timestamp tokens
                    for i in 0..vocab_size {
                        mask_buffer[i as usize] = if i >= timestamp_begin {
                            f32::NEG_INFINITY
                        } else {
                            0.0
                        };
                    }
                    masks.push(Tensor::new(mask_buffer.as_slice(), &device)?);
                } else {
                    // Cannot be normal text tokens - suppress everything before EOT
                    for i in 0..vocab_size {
                        mask_buffer[i as usize] = if i < self.eot_token {
                            f32::NEG_INFINITY
                        } else {
                            0.0
                        };
                    }
                    masks.push(Tensor::new(mask_buffer.as_slice(), &device)?);
                }
            }

            // ========== RULE 2: Non-decreasing timestamp constraint ==========
            // Timestamps shouldn't decrease; forbid timestamp tokens smaller than the last
            let timestamp_tokens: Vec<u32> = sampled_tokens
                .iter()
                .filter(|&&t| t >= timestamp_begin)
                .cloned()
                .collect();

            if !timestamp_tokens.is_empty() {
                let timestamp_last = if last_was_timestamp && !penultimate_was_timestamp {
                    *timestamp_tokens.last().unwrap()
                } else {
                    timestamp_tokens.last().unwrap() + 1
                };

                for i in 0..vocab_size {
                    mask_buffer[i as usize] = if i >= timestamp_begin && i < timestamp_last {
                        f32::NEG_INFINITY
                    } else {
                        0.0
                    };
                }
                masks.push(Tensor::new(mask_buffer.as_slice(), &device)?);
            }
        }

        // ========== RULE 3: Force initial timestamp ==========
        // At the beginning, suppress generating non-timestamp tokens
        if tokens.len() == sample_begin {
            for i in 0..vocab_size {
                mask_buffer[i as usize] = if i < timestamp_begin {
                    f32::NEG_INFINITY
                } else {
                    0.0
                };
            }
            masks.push(Tensor::new(mask_buffer.as_slice(), &device)?);

            // Apply the max_initial_timestamp constraint
            if let Some(max_initial_timestamp_index) = self.max_initial_timestamp_index {
                let last_allowed = timestamp_begin + max_initial_timestamp_index;
                if last_allowed < vocab_size {
                    for i in 0..vocab_size {
                        mask_buffer[i as usize] = if i > last_allowed {
                            f32::NEG_INFINITY
                        } else {
                            0.0
                        };
                    }
                    masks.push(Tensor::new(mask_buffer.as_slice(), &device)?);
                }
            }
        }

        // ========== APPLY MASKS: Apply all constraint masks ==========
        let mut logits = input_logits.clone();
        for mask in masks {
            logits = logits.broadcast_add(&mask)?;
        }

        // ========== RULE 4: Probability-based timestamp preference ==========
        // If sum of probability over timestamps is above any other token, sample timestamp
        let log_probs = log_softmax(&logits, 0)?;

        // Extract timestamp and text log probabilities
        let timestamp_log_probs = log_probs.narrow(
            0,
            timestamp_begin as usize,
            vocab_size as usize - timestamp_begin as usize,
        )?;

        let text_log_probs = log_probs.narrow(0, 0, timestamp_begin as usize)?;

        // Implement logsumexp for timestamp tokens (numerically stable)
        let timestamp_logprob = {
            let max_val = timestamp_log_probs.max(0)?;
            let shifted = timestamp_log_probs.broadcast_sub(&max_val)?;
            let exp_shifted = shifted.exp()?;
            let sum_exp = exp_shifted.sum(0)?;
            let log_sum = sum_exp.log()?;
            max_val.broadcast_add(&log_sum)?.to_scalar::<f32>()?
        };

        // Get max text token log probability
        let max_text_token_logprob: f32 = text_log_probs.max(0)?.to_scalar::<f32>()?;

        // Compare in log space
        if timestamp_logprob > max_text_token_logprob {
            // Only consider timestamp tokens
            for i in 0..vocab_size {
                mask_buffer[i as usize] = if i < timestamp_begin {
                    f32::NEG_INFINITY
                } else {
                    0.0
                };
            }
            let mask_tensor = Tensor::new(mask_buffer.as_slice(), &device)?;
            logits = logits.broadcast_add(&mask_tensor)?;
        }

        Ok(logits)
    }
}

pub fn token_id(tokenizer: &Tokenizer, token: &str) -> candle::Result<u32> {
    match tokenizer.token_to_id(token) {
        None => candle::bail!("no token-id for {token}"),
        Some(id) => Ok(id),
    }
}

/// gzip-style compression ratio of the text, as Whisper uses it: a loop
/// ("I'm I'm I'm ...") compresses far better than speech.
pub fn compression_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let packed = miniz_oxide::deflate::compress_to_vec(text.as_bytes(), 6);
    text.len() as f64 / packed.len().max(1) as f64
}
