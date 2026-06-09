// Builds chart payloads for the frontend (candles, overlays, drawings,
// position lines). Heavy aggregation stays here so React stays fast.

pub struct ChartPayloadBuilder;

impl ChartPayloadBuilder {
    pub fn new() -> Self {
        Self
    }
}
