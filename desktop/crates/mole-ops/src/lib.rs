// mole-ops: feature-level operations over the mole-core safety layer. Every
// destructive feature produces a DeletionPlan (consumed unchanged by preview
// and execute) and executes it exclusively through engine::execute → the
// mole-core sink.

pub mod analyze;
pub mod appmeta;
pub mod clean;
pub mod docker;
pub mod engine;
pub mod history;
pub mod installer;
pub mod optimize;
pub mod orphans;
pub mod purge;
pub mod scanutil;
pub mod smc;
pub mod status;
pub mod tools;
pub mod touchid;
pub mod trash;
pub mod uninstall;
pub mod updates;
pub mod whitelist;
