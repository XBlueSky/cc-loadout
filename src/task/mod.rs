//! Unified scheduled-run subsystem: prime (ping) and task (real prompt) share
//! one store (`tasks.json`), one execution engine, and one crontab block.

pub mod config;
pub mod exec;
pub mod ops;
pub mod resume;
pub mod run;
pub mod sandbox;
