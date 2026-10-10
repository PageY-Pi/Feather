//! hashline lib: engine + store + grep + AgentTool wiring.

pub mod engine;
pub mod grep;
pub mod store;
pub mod tools;

pub use engine::{Anchor, AnchorMap, AnchorRng, DiffKind, DiffRow, Direction, EngineError};
pub use grep::{anchor_grep, GrepHit, GrepOptions, GrepOutput};
pub use store::AnchorStore;
