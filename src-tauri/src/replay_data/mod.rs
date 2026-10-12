// Replay data, prepared from what the strategies declare they need.
//
// See `profile` for the contract. Each strategy that can be replayed registers its
// `ReplayDataNeeds` in `needs_registry`; everything downstream (planning, fetching,
// storing, reading back) works from the declared profiles only.

pub mod intervals;
pub mod plan;
pub mod prepare;
pub mod read;
pub mod profile;
pub mod store;

pub use profile::{
    DataProfile, EtWindow, Hit, ReplayDataNeeds, SymbolScanner, TickNeeds, UniverseFilter,
    WindowPad,
};

/// Every strategy whose replay data can be prepared, in display order.
pub fn needs_registry() -> Vec<Box<dyn ReplayDataNeeds>> {
    vec![Box::new(crate::micro_pullback::prescan::MicroPullbackNeeds)]
}

/// One registered strategy by id.
pub fn needs_for(id: &str) -> Option<Box<dyn ReplayDataNeeds>> {
    needs_registry().into_iter().find(|n| n.id() == id)
}
