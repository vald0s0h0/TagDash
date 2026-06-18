// Company-intel data providers. Each module is a self-contained client for one
// source, returning the normalized model types and never touching the DB — the
// orchestrator (super::run) owns sequencing, storage and the per-section
// source/error bookkeeping. Keeping providers DB-free is what lets the whole
// `company_intel` module lift cleanly into a standalone background worker later.

pub mod fmp_fin;
pub mod massive_si;
pub mod sec_edgar;
