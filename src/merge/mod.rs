// Grow-only set merge for JSONL session files.
// Implemented in US-006 (entry parsing + set-union merge) and
// US-007 (prefix verification and conflict detection).

pub mod entry;
pub mod set_union;
