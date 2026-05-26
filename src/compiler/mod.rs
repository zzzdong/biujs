pub mod codegen;
pub mod error;
pub mod ir;
pub mod lowering;
pub mod parser;
pub mod regalloc;
pub mod semantic;
pub mod symbol;

use std::collections::HashMap;

use crate::bytecode::{FunctionId, Module, Register};
use crate::compiler::error::CompileError;
use crate::compiler::ir::{FuncSignature, FunctionBuilder, IrFunction, IrUnit, SSABuilder};
use crate::compiler::lowering::JSASTLower;
use crate::compiler::symbol::SymbolTable;
use crate::compiler::parser::parse_js;

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
        let program = parse_js(&allocator, source).map_err(|parse_errors| {
            // Convert ParseErrors to CompileError, using the first error's location
            if let Some(first_err) = parse_errors.errors.first() {
                CompileError::syntax(
                    format!("{}", parse_errors),
                    first_err.line,
                    first_err.column
                )
            } else {
                CompileError::syntax("Parse error", 1, 1)
            }
        })?;

        // 2. Run semantic analysis
        let mut semantic = crate::compiler::semantic::SemanticAnalyzer::new();
        let semantic_errors = semantic.analyze(&program);
        if !semantic_errors.is_empty() {
            return Err(semantic_errors.into_iter().next().unwrap());
        }

        // 3. Create IR unit and main function
        let mut unit = IrUnit::new();
        let main_sig = FuncSignature::new("main", vec![]);
        let main_id = unit.declare_function(main_sig.clone());
        let mut main_func = IrFunction::new(main_id, main_sig);

        // 4. Lower AST -> IR
        {
            let mut builder = FunctionBuilder::new(&mut unit, &mut main_func);
            let symbols = SymbolTable::new();
            let mut lower = JSASTLower::new(&mut builder, symbols);
            lower.lower_program(&program);
        }
        // FunctionBuilder dropped, borrows released

        // 5. Define main function in unit
        unit.define_function(main_id, main_func);

        // 6. Run SSA + Codegen for ALL functions in the unit
        let registers = Register::general();
        let mut all_codes: Vec<crate::bytecode::Bytecode> = Vec::new();
        let mut symtab = HashMap::new();

        let function_count = unit.functions.len();
        for idx in 0..function_count {
            let func_id = FunctionId::new(idx as u32);

            // Run SSA conversion
            let throw_to_handlers = {
                let func = unit.get_function_mut(func_id).ok_or_else(|| {
                    CompileError::internal(format!("function {func_id} not found"))
                })?;
                let mut ssa = SSABuilder::new(&mut func.control_flow_graph);
                ssa.convert_to_ssa();
                ssa.into_throw_to_handlers()
            };

            // Extract CFG for codegen (move it out)
            let cfg = {
                let func = unit.get_function_mut(func_id).ok_or_else(|| {
                    CompileError::internal(format!("function {func_id} not found"))
                })?;
                std::mem::replace(&mut func.control_flow_graph, ir::ControlFlowGraph::new())
            };

            // Run codegen -> bytecode
            let mut codegen = Codegen::new(&registers, throw_to_handlers);
            let func_codes = codegen.generate_code(cfg).to_vec();

            // Record offset and append bytecodes
            let offset = all_codes.len();
            symtab.insert(func_id, offset);
            all_codes.extend(func_codes);
        }

        // 6. Build module with all functions
        Ok(Module::new(
            Some("main".to_string()),
            unit.constants,
            symtab,
            all_codes,
        ))
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}
