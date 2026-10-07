pub mod codegen;
pub mod error;
pub mod ir;
pub mod lowering;
pub mod parser;
pub mod regalloc;
pub mod semantic;
pub mod symbol;

use std::collections::HashMap;

use crate::bytecode::{CodeBlock, EhRegion, FnFlags, FunctionId, Module, Register};
use crate::compiler::error::CompileError;
use crate::compiler::ir::{FuncSignature, FunctionBuilder, IrFunction, IrUnit, SSABuilder};
use crate::compiler::lowering::JSASTLower;
use crate::compiler::parser::parse_js;
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
        let program = parse_js(&allocator, source).map_err(|parse_errors| {
            // Convert ParseErrors to CompileError, using the first error's location
            if let Some(first_err) = parse_errors.errors.first() {
                CompileError::syntax(
                    format!("{}", parse_errors),
                    first_err.line,
                    first_err.column,
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
            let mut lower = JSASTLower::new_script(&mut builder, symbols);
            lower.lower_program(&program);
        }
        // FunctionBuilder dropped, borrows released

        // 5. Define main function in unit
        unit.define_function(main_id, main_func);

        // 6. Run SSA + Codegen for ALL functions in the unit
        let registers = Register::general();
        let mut all_codes: Vec<crate::bytecode::Instr> = Vec::new();

        let function_count = unit.functions.len();
        // 每个函数的**声明属性**：先一次性读出来，免得与下面的可变借用来回交错。
        // 匿名函数的 `name` 是 `""`，不是 `Name` 的 *debug* `Display` 打的占位符。
        let metas: Vec<(String, usize, FnFlags)> = unit
            .functions
            .iter()
            .map(|func| {
                (
                    func.signature.name.0.clone().unwrap_or_default(),
                    func.signature.arity,
                    FnFlags {
                        generator: func.signature.is_generator,
                        r#async: func.signature.is_async,
                        derived_ctor: func.signature.is_derived_ctor,
                    },
                )
            })
            .collect();
        // EH 区域表：每个函数一份，**由 codegen 声明**（见 `bytecode::EhRegion` 的说明）。
        let mut eh_regions: HashMap<u32, Vec<EhRegion>> = HashMap::new();
        // 每个函数的描述：**下标 = 函数 id**（`ir` 层保证 0..n 连续）。
        let mut code_blocks: Vec<CodeBlock> = Vec::with_capacity(function_count);

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
            // 区域表是**块 id 解析成 pc** 之后的产物（pc 从 0 开始，属于本函数），
            // 所以要在下面按 `offset` 平移到整个模块的坐标系里。
            let func_eh: Vec<EhRegion> = codegen
                .eh_regions()
                .iter()
                .map(|r| EhRegion {
                    start: r.start,
                    exits: r.exits.clone(),
                    catch: r.catch,
                    finally: r.finally,
                })
                .collect();

            // Record offset and append bytecodes
            let offset = all_codes.len();
            eh_regions.insert(
                func_id.as_usize() as u32,
                func_eh
                    .into_iter()
                    .map(|r| EhRegion {
                        start: r.start + offset,
                        exits: r.exits.into_iter().map(|pc| pc + offset).collect(),
                        catch: r.catch.map(|pc| pc + offset),
                        finally: r.finally.map(|pc| pc + offset),
                    })
                    .collect(),
            );
            // The function's exit is its trailing `Ret` — the one the lowering
            // appends after sealing the last block. A suspended generator
            // resumed with a return completion re-enters here (see
            // `CodeBlock::exit_pc`).
            //
            // Deliberately `Opcode::Ret` and not `Kind::Return`: the module-level
            // code ends with `Halt`, which is also `Kind::Return` (`bytecode.rs`),
            // but its pc is not a function exit.
            let exit_pc = func_codes
                .iter()
                .rposition(|code| matches!(code.opcode(), crate::bytecode::Opcode::Ret))
                .map(|idx| offset + idx);
            let (name, arity, flags) = metas[idx].clone();
            code_blocks.push(CodeBlock {
                start: offset,
                name,
                arity,
                flags,
                exit_pc,
            });
            all_codes.extend(func_codes);
        }

        // 6. Build module with all functions
        Ok(Module::new(
            Some("main".to_string()),
            unit.constants,
            code_blocks,
            all_codes,
            eh_regions,
        ))
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}
