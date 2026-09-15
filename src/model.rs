use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Tokens {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

impl Tokens {
    // Cache and reasoning are subsets, never additional tokens.
    pub fn total(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
    pub fn valid(self) -> bool {
        self.cached_input_tokens
            .saturating_add(self.cache_write_input_tokens)
            <= self.input_tokens
            && self.reasoning_output_tokens <= self.output_tokens
            && self.input_tokens <= 1_000_000_000_000
            && self.output_tokens <= 1_000_000_000_000
    }
    pub fn add(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.cached_input_tokens += rhs.cached_input_tokens;
        self.cache_write_input_tokens += rhs.cache_write_input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.reasoning_output_tokens += rhs.reasoning_output_tokens;
    }
    pub fn delta(self, previous: Self) -> Option<Self> {
        Some(Self {
            input_tokens: self.input_tokens.checked_sub(previous.input_tokens)?,
            cached_input_tokens: self
                .cached_input_tokens
                .checked_sub(previous.cached_input_tokens)?,
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .checked_sub(previous.cache_write_input_tokens)?,
            output_tokens: self.output_tokens.checked_sub(previous.output_tokens)?,
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .checked_sub(previous.reasoning_output_tokens)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub project: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub at: DateTime<Utc>,
    pub model: String,
    pub effort: String,
    pub service_tier: String,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: String,
    pub ttft_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Annotation {
    pub run_id: String,
    pub label: String,
    pub benchmark: String,
    pub quality: Option<f64>,
    pub baseline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Price {
    pub provider: String,
    pub model: String,
    pub service_tier: String,
    pub effective_at: DateTime<Utc>,
    pub input_per_million: f64,
    pub cached_per_million: f64,
    pub cache_write_per_million: f64,
    pub output_per_million: f64,
    pub source: String,
}

impl Price {
    pub fn estimate(&self, tokens: Tokens) -> f64 {
        let uncached =
            tokens.input_tokens - tokens.cached_input_tokens - tokens.cache_write_input_tokens;
        (uncached as f64 * self.input_per_million
            + tokens.cached_input_tokens as f64 * self.cached_per_million
            + tokens.cache_write_input_tokens as f64 * self.cache_write_per_million
            + tokens.output_tokens as f64 * self.output_per_million)
            / 1_000_000.0
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub sessions: Vec<Session>,
    pub usage: Vec<Usage>,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub prices: Vec<Price>,
}
