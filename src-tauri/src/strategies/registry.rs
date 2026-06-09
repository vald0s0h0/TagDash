// Centralized strategy registry. Add each strategy here after creating its file.
// TODO: replace this manual list with a build.rs / proc-macro auto-discovery pass.

use std::sync::OnceLock;

use crate::strategies::{
    backside_parabolic::BacksideParabolic, micro_pullback::MicroPullback,
    panic_mean_reversion::PanicMeanReversion, perfect_pullback::PerfectPullback, ScanStrategy,
};

/// One shared instance of every compiled strategy, enabled or not. The strategies
/// are zero-sized, stateless structs, so they are built once and cached in a
/// `OnceLock` — repeated calls (scanner pass, commands, alarm watcher) hand back
/// the same slice instead of re-allocating seven boxes each time. The scanner
/// filters by `strategy.enabled()` at runtime.
pub fn all_strategies() -> &'static [Box<dyn ScanStrategy>] {
    static REGISTRY: OnceLock<Vec<Box<dyn ScanStrategy>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        vec![
            Box::new(MicroPullback) as Box<dyn ScanStrategy>,
            Box::new(BacksideParabolic),
            Box::new(PanicMeanReversion),
            Box::new(PerfectPullback),
        ]
    })
}
