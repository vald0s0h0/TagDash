// Claude / LLM client. Strictly async. Must never block scanner, alerts, UI,
// SL/TP placement, or 25/50/100/X buttons.

pub mod deepseek;

pub struct LlmClient;

impl LlmClient {
    pub fn new() -> Self {
        Self
    }
}
