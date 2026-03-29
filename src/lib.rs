// Chronicle library root.
//
// This crate is primarily a binary tool.  The library target exists solely to
// make integration tests in `tests/` possible.  All modules are re-exported
// as `pub` so the `tests/` crate can import the testable `_impl` helpers.
#![allow(dead_code)]

pub mod agents;
pub mod canon;
pub mod cli;
pub mod config;
pub mod errors;
pub mod git;
pub mod merge;
pub mod scan;
pub mod scheduler;
