//! Feature integration tests for biujs
//!
//! Each sub-module targets a specific family of JavaScript language features.
//! Helper functions for running JS and inspecting results are in `helpers`.

pub mod helpers;

#[path = "features/arithmetic_comparison.rs"]
mod arithmetic_comparison;
#[path = "features/arrow_functions.rs"]
mod arrow_functions;
#[path = "features/arrow_functions_return.rs"]
mod arrow_functions_return;
#[path = "features/builtins.rs"]
mod builtins_tests;
#[path = "features/complex.rs"]
mod complex;
#[path = "features/const_var_test.rs"]
mod const_var_test;
#[path = "features/control_flow.rs"]
mod control_flow;
#[path = "features/debug_instanceof.rs"]
mod debug_instanceof;
#[path = "features/debug_instanceof2.rs"]
mod debug_instanceof2;
#[path = "features/debug_propset.rs"]
mod debug_propset;
#[path = "features/functions.rs"]
mod functions;
#[path = "features/logical_typeof.rs"]
mod logical_typeof;
#[path = "features/objects_and_arrays.rs"]
mod objects_and_arrays;
#[path = "features/stack_overflow.rs"]
mod stack_overflow;
#[path = "features/symbol_test.rs"]
mod symbol_test;
#[path = "features/variables.rs"]
mod variables;
