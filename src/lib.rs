//! korin: a guided runbook language. Every command is shown before it runs.

pub mod ast;
pub mod compile;
pub mod diag;
pub mod fmt;
pub mod ir;
pub mod jq;
pub mod parser;
pub mod redact;
pub mod render;
pub mod run;
pub mod signals;
pub mod stdlib;
