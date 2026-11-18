//! completion specs: the declarative model, the help text parser and the helper that runs a
//! program's help. shared by the engine and the spec generator so there is one parser

pub mod help;
pub mod model;
pub mod runner;

pub use model::{Arg, Args, Names, Opt, Spec, Suggestion, Templates, merge_child, parse};
