use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Claude,
    Cursor,
    Grok,
}

impl Backend {
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor Agent",
            Self::Grok => "Grok CLI",
        }
    }
    pub fn from_cli_id(id: &str) -> Option<Self> {
        match id {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "grok" => Some(Self::Grok),
            _ => None,
        }
    }
    pub fn for_provider(provider: &str) -> Option<Self> {
        match provider {
            "openai" | "codex" => Some(Self::Codex),
            "anthropic" | "claude" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "xai" | "grok" => Some(Self::Grok),
            _ => None,
        }
    }
    pub fn default_provider(self) -> &'static str {
        match self {
            Self::Codex => "openai",
            Self::Claude => "anthropic",
            Self::Cursor => "cursor",
            Self::Grok => "xai",
        }
    }
    pub fn default_model(self) -> &'static str {
        if self == Self::Codex {
            "gpt-6-astra"
        } else {
            "default"
        }
    }
    pub fn default_effort(self) -> &'static str {
        if self == Self::Codex {
            "medium"
        } else {
            "default"
        }
    }
    pub fn command(self) -> &'static str {
        match self {
            Self::Cursor => "cursor-agent",
            _ => self.id(),
        }
    }
}
