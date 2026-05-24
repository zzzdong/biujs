pub mod codegen;
pub mod ir;
pub mod lowering;
pub mod regalloc;
pub mod symbol;

use std::collections::HashMap;

use crate::bytecode::{Module, Register};
use crate::compiler::ir::{
    FuncSignature, FunctionBuilder, IrFunction, IrUnit, SSABuilder,
};
use crate::compiler::lowering::{JSASTLower, parse_js};
use crate::compiler::symbol::SymbolTable;

use codegen::Codegen;
use oxc_allocator::Allocator;

/// JS Engine Compiler
///
/// Parses JavaScript source code and compiles it to bytecode.
pub struct Compiler {
    // TODO: Add compiler state
}

impl Compiler {
    pub fn new() -> Self {
        Self {}
    }

    /// Compile JavaScript source code to a bytecode module
    pub fn compile(&mut self, source: &str) -> Result<Module, CompileError> {
        // 1. Parse with oxc_parser -> AST
        let allocator = Allocator::default();
        let program = parse_js(&allocator, source);

        // 2. Create IR unit and main function
        let mut unit = IrUnit::new();
        let main_sig = FuncSignature::new("main", vec![]);
        let main_id = unit.declare_function(main_sig.clone());
        let mut main_func = IrFunction::new(main_id, main_sig);

        // 3. Lower AST -> IR
        {
            let mut builder = FunctionBuilder::new(&mut unit, &mut main_func);
            let symbols = SymbolTable::new();
            let mut lower = JSASTLower::new(&mut builder, symbols);
            lower.lower_program(&program);
        }
        // FunctionBuilder dropped, borrows released

        // 4. Define main function in unit
        unit.define_function(main_id, main_func);

        // 5. Run SSA conversion on main function
        let throw_to_handlers = {
            let main_func = unit.get_function_mut(main_id).unwrap();
            let mut ssa = SSABuilder::new(&mut main_func.control_flow_graph);
            ssa.convert_to_ssa();
            ssa.into_throw_to_handlers()
        };

        // 6. Extract CFG for codegen (move it out)
        let main_func = unit.get_function_mut(main_id).unwrap();
        let cfg = std::mem::replace(
            &mut main_func.control_flow_graph,
            ir::ControlFlowGraph::new(),
        );

        // 7. Run codegen -> bytecode
        let registers = Register::general();
        let mut codegen = Codegen::new(&registers, throw_to_handlers);
        let codes = codegen.generate_code(cfg).to_vec();

        // 8. Build module
        Ok(Module::new(
            Some("main".to_string()),
            unit.constants,
            HashMap::from([(main_id, 0)]),
            codes,
        ))
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum CompileError {
    NotImplemented,
    ParseError(String),
    LowerError(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::NotImplemented => write!(f, "compiler not yet implemented"),
            CompileError::ParseError(msg) => write!(f, "parse error: {msg}"),
            CompileError::LowerError(msg) => write!(f, "lower error: {msg}"),
        }
    }
}

impl std::error::Error for CompileError {}
