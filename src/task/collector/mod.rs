//! Collector task kind: the part of a scheduled run that hands the work to
//! Geralt's launcher instead of `claude -p`. Submodules are added task by task;
//! each one is self-contained and called from the collector run path.

pub mod export;
