//! JSASTLower: transforms oxc AST to IR.
use oxc_ast::ast::*;

use crate::bytecode::{Opcode, Primitive};
use crate::compiler::ir::{
    BlockId, FuncParam, FuncSignature, FunctionBuilder, InstBuilder, IrFunction, Name, Value,
};
use crate::compiler::symbol::SymbolTable;

/// Wraps a Value so it can be stored in the symbol table.
#[derive(Debug, Clone, Copy)]
pub struct Variable(pub Value);

impl Variable {
    pub fn new(addr: Value) -> Variable {
        Variable(addr)
    }
}

/// Context for break/continue inside loops.
struct LoopContext {
    break_point: BlockId,
    continue_point: BlockId,
    /// SEH depth when entering the loop (number of nested try blocks)
    seh_depth: usize,
}

impl LoopContext {
    fn new(break_point: BlockId, continue_point: BlockId, seh_depth: usize) -> Self {
        Self {
            break_point,
            continue_point,
            seh_depth,
        }
    }
}

/// Lowers oxc AST nodes into IR instructions.
pub struct JSASTLower<'a> {
    builder: &'a mut dyn InstBuilder,
    symbols: SymbolTable<Variable>,
    loop_contexts: Vec<LoopContext>,
    /// Current SEH depth (number of nested try blocks)
    seh_depth: usize,
    /// Arrow function this capture: if Some, contains the variable holding captured `this`
    arrow_this_var: Option<Value>,
}

impl<'a> JSASTLower<'a> {
    pub fn new(builder: &'a mut dyn InstBuilder, symbols: SymbolTable<Variable>) -> Self {
        Self {
            builder,
            symbols,
            loop_contexts: Vec::new(),
            seh_depth: 0,
            arrow_this_var: None,
        }
    }

    /// Lower a complete JS program (top-level).
    pub fn lower_program(&mut self, program: &Program<'_>) {
        // First pass: collect function declarations for hoisting
        // We can't alloc/assign yet because there's no current block
        let mut hoisted_funcs: Vec<(String, Value)> = Vec::new();
        for stmt in &program.body {
            if let Statement::FunctionDeclaration(func) = stmt {
                if let Some((name, val)) = self.collect_function_declaration(func) {
                    hoisted_funcs.push((name, val));
                }
            }
        }

        let entry = self.create_block("main");
        self.builder.switch_to_block(entry);
        self.builder.set_entry(entry);

        // Now assign hoisted functions to symbols (we have a current block)
        for (name, func_val) in hoisted_funcs {
            let dst = self.builder.alloc();
            self.builder.assign(dst, func_val);
            self.symbols.insert(name, Variable::new(dst));
        }

        // Lower directives (e.g. "use strict", or standalone string literals like "hello";)
        // oxc parses standalone string expression statements as Directive Prologues.
        let mut last_expr: Option<Value> = None;
        for directive in &program.directives {
            let val = self
                .builder
                .load_constant(directive.expression.value.as_str().into());
            last_expr = Some(val);
        }

        // Second pass: lower statements
        for stmt in &program.body {
            match stmt {
                // Function declarations are already hoisted, skip them
                Statement::FunctionDeclaration(_) => {}
                _ => {
                    last_expr = self.lower_statement_with_result(stmt);
                }
            }
        }

        // Ensure entry block has a terminator, passing last expression as return value
        match last_expr {
            Some(val) => self.builder.make_halt_with_value(val),
            None => self.builder.make_halt(),
        }
    }

    // ──────────────────────── Statement Lowering ────────────────────────

    /// Lower a statement, returning the result value if it's an expression statement.
    fn lower_statement_with_result(&mut self, stmt: &Statement<'_>) -> Option<Value> {
        match stmt {
            Statement::ExpressionStatement(expr_stmt) => {
                Some(self.lower_expression(&expr_stmt.expression))
            }
            Statement::TryStatement(try_stmt) => self.lower_try(try_stmt),
            _ => {
                self.lower_statement(stmt);
                None
            }
        }
    }

    fn lower_statement(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::VariableDeclaration(decl) => self.lower_variable_declaration(decl),
            Statement::ExpressionStatement(expr_stmt) => {
                self.lower_expression(&expr_stmt.expression);
            }
            Statement::ReturnStatement(ret) => self.lower_return(ret),
            Statement::IfStatement(if_stmt) => self.lower_if(if_stmt),
            Statement::WhileStatement(while_stmt) => self.lower_while(while_stmt),
            Statement::ForStatement(for_stmt) => self.lower_for(for_stmt),
            Statement::BlockStatement(block) => self.lower_block_stmt(block),
            Statement::BreakStatement(_) => self.lower_break(),
            Statement::ContinueStatement(_) => self.lower_continue(),
            Statement::ThrowStatement(throw) => self.lower_throw(throw),
            Statement::TryStatement(try_stmt) => {
                self.lower_try(try_stmt);
            }
            Statement::FunctionDeclaration(_) => {} // already hoisted
            Statement::ClassDeclaration(class) => self.lower_class_declaration(class),
            Statement::EmptyStatement(_) => {}
            _ => {
                log::warn!("unimplemented statement: {:?}", stmt);
            }
        }
    }

    fn lower_class_declaration(&mut self, class: &Class<'_>) {
        let class_val = self.lower_class(class);
        if let Some(id) = &class.id {
            let dst = self.builder.alloc();
            self.builder.assign(dst, class_val);
            self.symbols.insert(id.name.to_string(), Variable::new(dst));
        }
    }

    /// Lower a class definition (both declarations and expressions).
    /// Returns a FunctionObject-wrapped constructor with prototype methods.
    fn lower_class(&mut self, class: &Class<'_>) -> Value {
        let class_name = class
            .id
            .as_ref()
            .map(|id| id.name.to_string())
            .unwrap_or_else(|| "<class>".to_string());

        // 1. Separate constructor and method definitions
        // Collect info as owned strings to avoid borrow issues
        let mut constructor_idx: Option<usize> = None;
        let mut method_infos: Vec<(String, &Function)> = Vec::new();

        for element in &class.body.body {
            if let oxc_ast::ast::ClassElement::MethodDefinition(method_def) = element {
                if method_def.kind == oxc_ast::ast::MethodDefinitionKind::Constructor {
                    constructor_idx = Some(method_infos.len());
                    method_infos.push((String::new(), &method_def.value));
                } else if !method_def.r#static {
                    if let Some(name) = self.class_method_name_str(method_def) {
                        method_infos.push((name, &method_def.value));
                    }
                }
            }
        }

        // 2. Create prototype object
        let proto = self.builder.make_object();

        // 3. Process each method - create function and add to prototype or use as constructor
        let mut constructor_id = None;

        for (idx, (method_name, method_func)) in method_infos.iter().enumerate() {
            let params: Vec<FuncParam> = method_func
                .params
                .items
                .iter()
                .map(|p| FuncParam::new(self.binding_pattern_name(&p.pattern)))
                .collect();

            let is_constructor = Some(idx) == constructor_idx;

            let func_val = self.lower_function_inner(
                Some(if is_constructor {
                    format!("{}_{}", class_name, "constructor")
                } else {
                    format!("{}.{}", class_name, method_name)
                }),
                params,
                method_func.body.as_ref().unwrap(),
                None,
                false,
                &[],
            );

            if is_constructor {
                constructor_id = Some(func_val);
            } else {
                self.builder.set_property(proto, method_name, func_val);
            }
        }

        // 4. If no constructor, create a default one
        let constructor_id = constructor_id.unwrap_or_else(|| {
            let func_sig = FuncSignature::new(format!("{}_{}", class_name, "constructor"), vec![]);
            let func_id = self.builder.module_mut().declare_function(func_sig.clone());
            let mut func = IrFunction::new(func_id, func_sig);
            let symbols = self.symbols.clone();
            let mut func_builder = FunctionBuilder::new(self.builder.module_mut(), &mut func);
            let mut func_lower = JSASTLower::new(&mut func_builder, symbols);
            let entry = func_lower.create_block("default_ctor");
            func_lower.builder.set_entry(entry);
            func_lower.builder.switch_to_block(entry);
            func_lower.builder.return_(None);
            func_lower
                .builder
                .seal_block(func_lower.builder.current_block());
            self.builder.module_mut().define_function(func_id, func);
            Value::Function(func_id)
        });

        // 5. Create a FunctionObject wrapping the constructor
        let func_obj = self.builder.make_func_obj(constructor_id);

        // 6. Set .prototype on the constructor FunctionObject
        self.builder.set_property(func_obj, "prototype", proto);

        func_obj
    }

    fn class_method_name_str(&self, method: &MethodDefinition<'_>) -> Option<String> {
        match &method.key {
            oxc_ast::ast::PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
            oxc_ast::ast::PropertyKey::StringLiteral(lit) => Some(lit.value.to_string()),
            _ => {
                log::warn!("computed method name not supported in class");
                None
            }
        }
    }

    fn lower_variable_declaration(&mut self, decl: &VariableDeclaration<'_>) {
        for declarator in &decl.declarations {
            let name = self.binding_pattern_name(&declarator.id);
            let dst = self.builder.alloc();

            if let Some(init) = &declarator.init {
                let value = self.lower_expression(init);
                self.builder.assign(dst, value);
            }
            // else: dst stays as default (undefined)

            self.symbols.insert(name, Variable::new(dst));
        }
    }

    fn lower_return(&mut self, ret: &ReturnStatement<'_>) {
        let value = ret
            .argument
            .as_ref()
            .map(|expr| self.lower_expression(expr));
        self.builder.return_(value);
    }

    fn lower_if(&mut self, if_stmt: &IfStatement<'_>) {
        let merge_blk = self.create_block("if_merge");
        let then_blk = self.create_block("if_then");

        let cond = self.lower_expression(&if_stmt.test);
        let else_blk = if if_stmt.alternate.is_some() {
            self.create_block("if_else")
        } else {
            merge_blk
        };

        self.builder.br_if(cond, then_blk, else_blk);

        // Then branch
        self.builder.switch_to_block(then_blk);
        self.lower_statement(&if_stmt.consequent);
        if !self.current_block_is_terminated() {
            self.builder.jump(merge_blk);
        }

        // Else branch
        if let Some(alt) = &if_stmt.alternate {
            self.builder.switch_to_block(else_blk);
            self.lower_statement(alt);
            if !self.current_block_is_terminated() {
                self.builder.jump(merge_blk);
            }
        }

        self.builder.switch_to_block(merge_blk);
    }

    fn lower_while(&mut self, while_stmt: &WhileStatement<'_>) {
        let cond_blk = self.create_block("while_cond");
        let body_blk = self.create_block("while_body");
        let after_blk = self.create_block("while_after");

        self.enter_loop_context(after_blk, cond_blk);

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);

        let cond = self.lower_expression(&while_stmt.test);
        self.builder.br_if(cond, body_blk, after_blk);

        self.builder.switch_to_block(body_blk);
        self.lower_statement(&while_stmt.body);
        if !self.current_block_is_terminated() {
            self.builder.jump(cond_blk);
        }

        self.leave_loop_context();
        self.builder.switch_to_block(after_blk);
    }

    fn lower_for(&mut self, for_stmt: &ForStatement<'_>) {
        // ForStatement in oxc: for (init; test; update) body
        let cond_blk = self.create_block("for_cond");
        let body_blk = self.create_block("for_body");
        let update_blk = self.create_block("for_update");
        let after_blk = self.create_block("for_after");

        self.enter_loop_context(after_blk, update_blk);

        // Init
        if let Some(init) = &for_stmt.init {
            match init {
                ForStatementInit::VariableDeclaration(decl) => {
                    self.lower_variable_declaration(decl);
                }
                _ => {
                    if let Some(expr) = init.as_expression() {
                        self.lower_expression(expr);
                    }
                }
            }
        }

        self.builder.jump(cond_blk);

        // Condition
        self.builder.switch_to_block(cond_blk);
        if let Some(test) = &for_stmt.test {
            let cond = self.lower_expression(test);
            self.builder.br_if(cond, body_blk, after_blk);
        } else {
            self.builder.jump(body_blk);
        }

        // Body
        self.builder.switch_to_block(body_blk);
        self.lower_statement(&for_stmt.body);
        if !self.current_block_is_terminated() {
            self.builder.jump(update_blk);
        }

        // Update
        self.builder.switch_to_block(update_blk);
        if let Some(update) = &for_stmt.update {
            self.lower_expression(update);
        }
        self.builder.jump(cond_blk);

        self.leave_loop_context();
        self.builder.switch_to_block(after_blk);
    }

    fn lower_block_stmt(&mut self, block: &BlockStatement<'_>) {
        self.symbols.enter_scope();
        for stmt in &block.body {
            self.lower_statement(stmt);
            if self.current_block_is_terminated() {
                break;
            }
        }
        self.symbols.leave_scope();
    }

    fn lower_break(&mut self) {
        if let Some(ctx) = self.loop_contexts.last() {
            let break_point = ctx.break_point;
            // Check if we need to execute finally blocks before breaking
            let seh_depth_to_pop = self.seh_depth.saturating_sub(ctx.seh_depth);
            if seh_depth_to_pop > 0 {
                // Use delayed jump to execute finally blocks first
                self.builder.delayed_jump(break_point, seh_depth_to_pop);
            } else {
                self.builder.jump(break_point);
            }
            self.builder.seal_block(self.builder.current_block());
        } else {
            log::warn!("break outside loop - ignoring");
        }
    }

    fn lower_continue(&mut self) {
        if let Some(ctx) = self.loop_contexts.last() {
            let continue_point = ctx.continue_point;
            // Check if we need to execute finally blocks before continuing
            let seh_depth_to_pop = self.seh_depth.saturating_sub(ctx.seh_depth);
            if seh_depth_to_pop > 0 {
                // Use delayed jump to execute finally blocks first
                self.builder.delayed_jump(continue_point, seh_depth_to_pop);
            } else {
                self.builder.jump(continue_point);
            }
            self.builder.seal_block(self.builder.current_block());
        } else {
            log::warn!("continue outside loop - ignoring");
        }
    }

    fn lower_throw(&mut self, throw: &ThrowStatement<'_>) {
        let val = match &throw.argument {
            Expression::StringLiteral(lit) => {
                self.builder
                    .make_constant(crate::bytecode::Constant::String(
                        lit.value.to_string().into(),
                    ))
            }
            Expression::NumericLiteral(lit) => Value::Primitive(Primitive::Float(lit.value)),
            Expression::BooleanLiteral(lit) => Value::Primitive(Primitive::Boolean(lit.value)),
            Expression::NullLiteral(_) => Value::Primitive(Primitive::Null),
            _ => self.lower_expression(&throw.argument),
        };
        self.builder.throw_value(val);
        self.builder.seal_block(self.builder.current_block());
    }

    fn lower_try(&mut self, try_stmt: &TryStatement<'_>) -> Option<Value> {
        // Variable that accumulates the result of the try (or catch) block so that
        // the whole try statement can act as an expression and return its final value.
        let result_var = self.builder.alloc();
        self.builder.assign(
            result_var,
            Value::Primitive(crate::bytecode::Primitive::Undefined),
        );

        let try_body = self.create_block("try_body");
        let catch_blk = self.create_block("catch");
        let finally_blk = self.create_block("finally");
        let after_finally = self.create_block("after_finally");

        // Determine catch and finally blocks
        let has_catch = try_stmt.handler.is_some();
        let has_finally = try_stmt.finalizer.is_some();

        // The SEH handler is catch if present, otherwise finally
        let seh_handler = if has_catch { catch_blk } else { finally_blk };
        let seh_finally = if has_finally { Some(finally_blk) } else { None };

        // Increment SEH depth for try body
        self.seh_depth += 1;

        self.builder.push_seh(seh_handler, seh_finally);
        self.builder.add_exception_edge(try_body, seh_handler);
        if has_finally {
            self.builder.add_exception_edge(try_body, finally_blk);
        }
        self.builder.jump(try_body);

        // Try body
        self.builder.switch_to_block(try_body);
        if let Some(v) = self.lower_block_like_with_result(&try_stmt.block.body) {
            self.builder.assign(result_var, v);
        }
        // Decrement SEH depth after try body
        self.seh_depth -= 1;
        if !self.current_block_is_terminated() {
            self.builder.switch_to_block(try_body);
            self.builder.pop_seh();
            if has_finally {
                self.builder.jump(finally_blk);
            } else {
                self.builder.jump(after_finally);
            }
        }

        // Catch handler (if present)
        if has_catch {
            self.builder.switch_to_block(catch_blk);
            // Note: SEH record is NOT popped here because catch might throw again
            // and we need the finally block to execute in that case.
            // PopSeh is done in the normal flow before jumping to finally/after.
            let exc_val = self.builder.load_exception();
            if let Some(catch_clause) = &try_stmt.handler {
                self.symbols.enter_scope();
                let name = self.catch_clause_param_name(catch_clause);
                let dst = self.builder.alloc();
                self.builder.assign(dst, exc_val);
                self.symbols.insert(name, Variable::new(dst));
                if let Some(v) = self.lower_block_like_with_result(&catch_clause.body.body) {
                    self.builder.assign(result_var, v);
                }
                self.symbols.leave_scope();
            }
            if !self.current_block_is_terminated() {
                // Pop SEH before leaving catch normally
                self.builder.pop_seh();
                if has_finally {
                    self.builder.jump(finally_blk);
                } else {
                    self.builder.jump(after_finally);
                }
            }
        }

        // Finally handler (if present)
        if has_finally {
            self.builder.switch_to_block(finally_blk);
            self.lower_block_like(&try_stmt.finalizer.as_ref().unwrap().body);
            if !self.current_block_is_terminated() {
                // After finally, check if there's a pending exception to re-throw
                self.builder.resume_exception();
                self.builder.jump(after_finally);
            }
        }

        self.builder.switch_to_block(after_finally);
        Some(result_var)
    }

    // ──────────────────────── Expression Lowering ────────────────────────

    fn lower_expression(&mut self, expr: &Expression<'_>) -> Value {
        match expr {
            Expression::NumericLiteral(lit) => self.lower_numeric_literal(lit),
            Expression::StringLiteral(lit) => self.lower_string_literal(lit),
            Expression::BooleanLiteral(lit) => self.lower_boolean_literal(lit),
            Expression::NullLiteral(_) => Value::Primitive(Primitive::Null),
            Expression::Identifier(ident) => self.lower_identifier(ident),
            Expression::BinaryExpression(bin) => self.lower_binary(bin),
            Expression::UnaryExpression(unary) => self.lower_unary(unary),
            Expression::UpdateExpression(update) => self.lower_update(update),
            Expression::LogicalExpression(logical) => self.lower_logical(logical),
            Expression::AssignmentExpression(assign) => self.lower_assignment(assign),
            Expression::CallExpression(call) => self.lower_call(call),
            Expression::NewExpression(new) => self.lower_new(new),
            Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_) => self.lower_member_expr(expr),
            Expression::ConditionalExpression(cond) => self.lower_conditional(cond),
            Expression::ArrayExpression(arr) => self.lower_array(arr),
            Expression::ObjectExpression(obj) => self.lower_object(obj),
            Expression::ArrowFunctionExpression(arrow) => self.lower_arrow_function(arrow),
            Expression::FunctionExpression(func) => self.lower_function_expr(func),
            Expression::SequenceExpression(seq) => self.lower_sequence(seq),
            Expression::TemplateLiteral(tpl) => self.lower_template_literal(tpl),
            Expression::ParenthesizedExpression(paren) => self.lower_expression(&paren.expression),
            Expression::ThisExpression(_) => {
                // If inside an arrow function with captured this, use the captured value
                if let Some(captured_this) = self.arrow_this_var {
                    captured_this
                } else {
                    self.builder.load_this()
                }
            }
            Expression::ClassExpression(class) => self.lower_class(class),
            _ => {
                log::warn!("unimplemented expression: {:?}", expr);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn lower_numeric_literal(&mut self, lit: &NumericLiteral<'_>) -> Value {
        // JS uses f64 for all numbers
        Value::Primitive(Primitive::Float(lit.value))
    }

    fn lower_string_literal(&mut self, lit: &StringLiteral<'_>) -> Value {
        self.builder.load_constant(lit.value.as_str().into())
    }

    fn lower_boolean_literal(&mut self, lit: &BooleanLiteral) -> Value {
        Value::Primitive(Primitive::Boolean(lit.value))
    }

    fn lower_identifier(&mut self, ident: &IdentifierReference<'_>) -> Value {
        // Handle special identifiers
        match ident.name.as_str() {
            "undefined" => return Value::Primitive(Primitive::Undefined),
            "NaN" => return Value::Primitive(Primitive::Float(f64::NAN)),
            "Infinity" => return Value::Primitive(Primitive::Float(f64::INFINITY)),
            _ => {}
        }

        if let Some(var) = self.symbols.lookup(ident.name.as_str()) {
            var.0
        } else {
            // Try loading as external/global variable
            self.builder.load_external_variable(ident.name.to_string())
        }
    }

    fn lower_binary(&mut self, bin: &BinaryExpression<'_>) -> Value {
        let lhs = self.lower_expression(&bin.left);
        let rhs = self.lower_expression(&bin.right);

        match bin.operator {
            BinaryOperator::Addition => self.builder.binop(Opcode::Addx, lhs, rhs),
            BinaryOperator::Subtraction => self.builder.binop(Opcode::Subx, lhs, rhs),
            BinaryOperator::Multiplication => self.builder.binop(Opcode::Mulx, lhs, rhs),
            BinaryOperator::Division => self.builder.binop(Opcode::Divx, lhs, rhs),
            BinaryOperator::Remainder => self.builder.binop(Opcode::Remx, lhs, rhs),
            BinaryOperator::Equality => self.builder.binop(Opcode::Equal, lhs, rhs),
            BinaryOperator::Inequality => self.builder.binop(Opcode::NotEqual, lhs, rhs),
            BinaryOperator::StrictEquality => self.builder.binop(Opcode::StrictEqual, lhs, rhs),
            BinaryOperator::StrictInequality => {
                self.builder.binop(Opcode::StrictNotEqual, lhs, rhs)
            }
            BinaryOperator::LessThan => self.builder.binop(Opcode::Less, lhs, rhs),
            BinaryOperator::LessEqualThan => self.builder.binop(Opcode::LessEqual, lhs, rhs),
            BinaryOperator::GreaterThan => self.builder.binop(Opcode::Greater, lhs, rhs),
            BinaryOperator::GreaterEqualThan => self.builder.binop(Opcode::GreaterEqual, lhs, rhs),
            BinaryOperator::Instanceof => self.builder.binop(Opcode::InstanceOf, lhs, rhs),
            BinaryOperator::In => self.builder.binop(Opcode::In, lhs, rhs),
            _ => {
                log::warn!("unimplemented binary operator: {:?}", bin.operator);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn lower_unary(&mut self, unary: &UnaryExpression<'_>) -> Value {
        let arg = self.lower_expression(&unary.argument);

        match unary.operator {
            UnaryOperator::UnaryNegation => self.builder.unaryop(Opcode::Neg, arg),
            UnaryOperator::UnaryPlus => {
                // +expr → ToNumber(expr) — use Addx with 0
                let zero = Value::Primitive(Primitive::Float(0.0));
                self.builder.binop(Opcode::Addx, zero, arg)
            }
            UnaryOperator::LogicalNot => self.builder.unaryop(Opcode::Not, arg),
            UnaryOperator::BitwiseNot => {
                // ~expr → bitwise NOT
                self.builder.unaryop(Opcode::BitNot, arg)
            }
            UnaryOperator::Typeof => self.builder.typeof_(arg),
            UnaryOperator::Void => {
                // void expr: evaluate expr, return undefined
                self.lower_expression(&unary.argument);
                Value::Primitive(Primitive::Undefined)
            }
            UnaryOperator::Delete => {
                // delete is complex; placeholder
                log::warn!("delete operator not yet implemented");
                Value::Primitive(Primitive::Boolean(false))
            }
        }
    }

    fn lower_update(&mut self, update: &UpdateExpression<'_>) -> Value {
        let one = Value::Primitive(Primitive::Float(1.0));

        // Read current value and compute new value based on argument type
        let (arg, target_val) = match &update.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
                let var = self.symbols.lookup(ident.name.as_str());
                let current = var
                    .map(|v| v.0)
                    .unwrap_or(Value::Primitive(Primitive::Null));
                (current, var.map(|v| v.0))
            }
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let current = self
                    .builder
                    .get_property(object, member.property.name.as_str());
                (current, None) // complex target, simplified for now
            }
            SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                let current = self.builder.index_get(object, index);
                (current, None)
            }
            _ => {
                log::warn!(
                    "unsupported update expression target: {:?}",
                    update.argument
                );
                (Value::Primitive(Primitive::Null), None)
            }
        };

        let new_val = match update.operator {
            UpdateOperator::Increment => self.builder.binop(Opcode::Addx, arg, one),
            UpdateOperator::Decrement => self.builder.binop(Opcode::Subx, arg, one),
        };

        // Assign back to the target
        if let Some(dst) = target_val {
            self.builder.assign(dst, new_val);
        }

        // prefix returns new value, postfix returns old value
        if update.prefix { new_val } else { arg }
    }

    fn lower_logical(&mut self, logical: &LogicalExpression<'_>) -> Value {
        let lhs = self.lower_expression(&logical.left);

        match logical.operator {
            LogicalOperator::And => {
                // Short-circuit: if lhs is falsy, return lhs; else evaluate rhs
                let result = self.builder.alloc();
                let rhs_blk = self.create_block("and_rhs");
                let merge_blk = self.create_block("and_merge");

                // Always set result = lhs first. Then:
                //   truthy → rhs_blk (result gets overwritten with rhs)
                //   falsy  → merge_blk (result stays as lhs)
                self.builder.assign(result, lhs);
                self.builder.br_if(lhs, rhs_blk, merge_blk);

                self.builder.switch_to_block(rhs_blk);
                let rhs = self.lower_expression(&logical.right);
                self.builder.assign(result, rhs);
                self.builder.jump(merge_blk);

                self.builder.switch_to_block(merge_blk);
                result
            }
            LogicalOperator::Or => {
                // Short-circuit: if lhs is truthy, return lhs; else evaluate rhs
                let result = self.builder.alloc();
                let rhs_blk = self.create_block("or_rhs");
                let merge_blk = self.create_block("or_merge");

                // Always set result = lhs first. Then:
                //   truthy → merge_blk (result stays as lhs)
                //   falsy  → rhs_blk  (result gets overwritten with rhs)
                self.builder.assign(result, lhs);
                self.builder.br_if(lhs, merge_blk, rhs_blk);

                self.builder.switch_to_block(rhs_blk);
                let rhs = self.lower_expression(&logical.right);
                self.builder.assign(result, rhs);
                self.builder.jump(merge_blk);

                self.builder.switch_to_block(merge_blk);
                result
            }
            LogicalOperator::Coalesce => {
                // ??: if lhs is null/undefined, evaluate rhs
                // For now, simplified
                log::warn!("?? operator not fully implemented");
                lhs
            }
        }
    }

    fn lower_assignment(&mut self, assign: &AssignmentExpression<'_>) -> Value {
        let value = self.lower_expression(&assign.right);

        match &assign.left {
            AssignmentTarget::AssignmentTargetIdentifier(ident) => {
                let target = self.symbols.lookup(ident.name.as_str());
                match assign.operator {
                    AssignmentOperator::Assign => {
                        if let Some(var) = target {
                            self.builder.assign(var.0, value);
                        }
                    }
                    AssignmentOperator::Addition => {
                        if let Some(var) = target {
                            let result = self.builder.binop(Opcode::Addx, var.0, value);
                            self.builder.assign(var.0, result);
                        }
                    }
                    AssignmentOperator::Subtraction => {
                        if let Some(var) = target {
                            let result = self.builder.binop(Opcode::Subx, var.0, value);
                            self.builder.assign(var.0, result);
                        }
                    }
                    AssignmentOperator::Multiplication => {
                        if let Some(var) = target {
                            let result = self.builder.binop(Opcode::Mulx, var.0, value);
                            self.builder.assign(var.0, result);
                        }
                    }
                    AssignmentOperator::Division => {
                        if let Some(var) = target {
                            let result = self.builder.binop(Opcode::Divx, var.0, value);
                            self.builder.assign(var.0, result);
                        }
                    }
                    AssignmentOperator::Remainder => {
                        if let Some(var) = target {
                            let result = self.builder.binop(Opcode::Remx, var.0, value);
                            self.builder.assign(var.0, result);
                        }
                    }
                    _ => {
                        log::warn!("unimplemented assignment operator: {:?}", assign.operator);
                    }
                }
                value
            }
            AssignmentTarget::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                self.builder.index_set(object, index, value);
                value
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                self.builder
                    .set_property(object, member.property.name.as_str(), value);
                value
            }
            _ => {
                log::warn!("unimplemented assignment target: {:?}", assign.left);
                value
            }
        }
    }

    fn lower_call(&mut self, call: &CallExpression<'_>) -> Value {
        let args: Vec<Value> = call
            .arguments
            .iter()
            .map(|arg| match arg {
                Argument::SpreadElement(_) => {
                    log::warn!("spread arguments not yet supported");
                    Value::Primitive(Primitive::Null)
                }
                _ => {
                    // oxc Argument enum variants need matching
                    // For now, try to extract the expression
                    self.lower_argument_expr(arg)
                }
            })
            .collect();

        // Check if callee is a member expression (method call)
        match &call.callee {
            Expression::StaticMemberExpression(static_member) => {
                let object = self.lower_expression(&static_member.object);
                let prop_name = static_member.property.name.as_str();
                return self.builder.call_property(object, prop_name, args);
            }
            Expression::ComputedMemberExpression(computed_member) => {
                let object = self.lower_expression(&computed_member.object);
                let prop = self.lower_expression(&computed_member.expression);
                return self.builder.call_property_dynamic(object, prop, args);
            }
            _ => {}
        }

        // For non-member calls, lower the callee normally
        let callee = self.lower_expression(&call.callee);

        // Check if this is a call to a known built-in constructor/function
        // by examining the identifier name in the callee
        let is_builtin = self.is_builtin_call(&call.callee);

        if is_builtin {
            self.builder.make_call_native(callee, args)
        } else {
            self.builder.make_call(callee, args)
        }
    }

    fn lower_new(&mut self, new: &NewExpression<'_>) -> Value {
        let constructor = self.lower_expression(&new.callee);
        let args: Vec<Value> = new
            .arguments
            .iter()
            .map(|arg| self.lower_argument_expr(arg))
            .collect();

        self.builder.new_(constructor, args)
    }

    fn lower_member_expr(&mut self, expr: &Expression<'_>) -> Value {
        match expr {
            Expression::StaticMemberExpression(static_member) => {
                let object = self.lower_expression(&static_member.object);
                self.builder
                    .get_property(object, static_member.property.name.as_str())
            }
            Expression::ComputedMemberExpression(computed) => {
                let object = self.lower_expression(&computed.object);
                let index = self.lower_expression(&computed.expression);
                self.builder.index_get(object, index)
            }
            Expression::PrivateFieldExpression(_) => {
                log::warn!("private field expression not supported");
                Value::Primitive(Primitive::Null)
            }
            _ => {
                log::warn!("unexpected member expression variant: {:?}", expr);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn lower_conditional(&mut self, cond: &ConditionalExpression<'_>) -> Value {
        let result = self.builder.alloc();
        let then_blk = self.create_block("cond_then");
        let else_blk = self.create_block("cond_else");
        let merge_blk = self.create_block("cond_merge");

        let test = self.lower_expression(&cond.test);
        self.builder.br_if(test, then_blk, else_blk);

        self.builder.switch_to_block(then_blk);
        let then_val = self.lower_expression(&cond.consequent);
        self.builder.assign(result, then_val);
        self.builder.jump(merge_blk);

        self.builder.switch_to_block(else_blk);
        let else_val = self.lower_expression(&cond.alternate);
        self.builder.assign(result, else_val);
        self.builder.jump(merge_blk);

        self.builder.switch_to_block(merge_blk);
        result
    }

    fn lower_array(&mut self, arr: &ArrayExpression<'_>) -> Value {
        let array = self.builder.make_array();
        for element in &arr.elements {
            match element {
                ArrayExpressionElement::SpreadElement(_) => {
                    log::warn!("spread in array not yet supported");
                }
                ArrayExpressionElement::Elision(_) => {
                    // holes in array → push undefined
                    let undef = Value::Primitive(Primitive::Undefined);
                    self.builder.array_push(array, undef);
                }
                _ => {
                    let elem = self.lower_expression_or_null(element);
                    self.builder.array_push(array, elem);
                }
            }
        }
        array
    }

    fn lower_object(&mut self, obj: &ObjectExpression<'_>) -> Value {
        let object = self.builder.make_object();
        for prop in &obj.properties {
            match prop {
                ObjectPropertyKind::ObjectProperty(p) => {
                    let key = self.property_key_to_string(&p.key);
                    let value = self.lower_expression(&p.value);
                    self.builder.set_property(object, &key, value);
                }
                ObjectPropertyKind::SpreadProperty(_) => {
                    log::warn!("spread in object not yet supported");
                }
            }
        }
        object
    }

    fn lower_arrow_function(&mut self, arrow: &ArrowFunctionExpression<'_>) -> Value {
        let name = "<arrow>".to_string();
        let params: Vec<FuncParam> = arrow
            .params
            .items
            .iter()
            .map(|p| FuncParam::new(self.binding_pattern_name(&p.pattern)))
            .collect();

        // Arrow functions with expression body need automatic return
        let is_expression_body = arrow.expression;

        // Determine which outer variables need to be captured.
        // We collect all referenced identifiers and subtract those declared inside the arrow body.
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.to_string()).collect();
        let mut referenced = std::collections::HashSet::new();
        let mut declared = std::collections::HashSet::new();
        // Parameters are considered "declared" locally
        for p in &param_names {
            declared.insert(p.clone());
        }
        self.collect_free_idents_from_body(&arrow.body, &mut referenced, &mut declared);
        let free_idents: std::collections::HashSet<&str> = referenced
            .iter()
            .filter(|name| !declared.contains(**name))
            .copied()
            .collect();

        // Lower the arrow function body
        let func_val = self.lower_function_inner(
            Some(name),
            params,
            &arrow.body,
            None,
            is_expression_body,
            &free_idents
                .iter()
                .map(|s| String::from(*s))
                .collect::<Vec<String>>(),
        );

        // At runtime, capture the current `this` value
        let captured_this = self.builder.load_this();

        // Capture each free variable that exists in the current symbol table
        for name in &free_idents {
            if let Some(var) = self.symbols.lookup(name) {
                let name_const = self
                    .builder
                    .make_constant(crate::bytecode::Constant::String(std::sync::Arc::new(
                        String::from(*name),
                    )));
                let value = var.0;
                self.builder.closure_var(name_const, value);
            }
        }

        // Create arrow function object with captured `this` and captured vars
        self.builder.make_arrow_func_obj(func_val, captured_this)
    }

    fn lower_function_expr(&mut self, func: &Function<'_>) -> Value {
        let name = func
            .id
            .as_ref()
            .map(|id| id.name.to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        let params: Vec<FuncParam> = func
            .params
            .items
            .iter()
            .map(|p| FuncParam::new(self.binding_pattern_name(&p.pattern)))
            .collect();

        if let Some(body) = &func.body {
            self.lower_function_inner(Some(name), params, body, None, false, &[])
        } else {
            Value::Primitive(Primitive::Null)
        }
    }

    fn lower_sequence(&mut self, seq: &SequenceExpression<'_>) -> Value {
        let mut result = Value::Primitive(Primitive::Null);
        for expr in &seq.expressions {
            result = self.lower_expression(expr);
        }
        result
    }

    fn lower_template_literal(&mut self, tpl: &TemplateLiteral<'_>) -> Value {
        // Simplified: just concatenate string parts
        // For template literals with expressions, this is more complex
        let mut parts: Vec<Value> = Vec::new();
        for quasi in &tpl.quasis {
            let raw = quasi.value.raw.as_str();
            if !raw.is_empty() {
                parts.push(self.builder.load_constant(raw.into()));
            }
        }

        if parts.is_empty() {
            return Value::Primitive(Primitive::Null);
        }

        if parts.len() == 1 {
            return parts[0];
        }

        // Concatenate all parts using Addx
        let mut result = parts[0];
        for part in &parts[1..] {
            result = self.builder.binop(Opcode::Addx, result, *part);
        }
        result
    }

    // ──────────────────────── Function Lowering ────────────────────────

    /// Collect a function declaration for hoisting (returns name and function value).
    fn collect_function_declaration(&mut self, func: &Function<'_>) -> Option<(String, Value)> {
        let name = func
            .id
            .as_ref()
            .map(|id| id.name.to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        let params: Vec<FuncParam> = func
            .params
            .items
            .iter()
            .map(|p| FuncParam::new(self.binding_pattern_name(&p.pattern)))
            .collect();

        if let Some(body) = &func.body {
            let func_id_val =
                self.lower_function_inner(Some(name.clone()), params, body, None, false, &[]);
            Some((name, func_id_val))
        } else {
            None
        }
    }

    /// Lower a function into a new IrFunction and return its Value.
    /// For arrow functions:
    /// - `captured_this` contains the lexically captured `this` value
    /// - `auto_return` indicates if the body is an expression that should be auto-returned
    /// - `captured_names` lists variable names captured by closure — these will be excluded from
    ///   the inner function's symbol table so they resolve via LoadEnv at runtime
    fn lower_function_inner(
        &mut self,
        name: Option<String>,
        params: Vec<FuncParam>,
        body: &FunctionBody<'_>,
        captured_this: Option<Value>,
        auto_return: bool,
        captured_names: &[String],
    ) -> Value {
        // During hoisting, there may be no current block yet
        let curr = self.builder.try_current_block();

        let func_sig = FuncSignature::new(name.clone(), params.clone());
        let func_id = self.builder.module_mut().declare_function(func_sig.clone());

        let mut func = IrFunction::new(func_id, func_sig);

        // Clone outer symbols but exclude captured names so they fall through to LoadEnv
        let mut symbols = self.symbols.clone();
        for name in captured_names {
            symbols.remove(name);
        }

        let mut func_builder = FunctionBuilder::new(self.builder.module_mut(), &mut func);
        let mut func_lower = JSASTLower::new(&mut func_builder, symbols);

        // Set up arrow function this capture if provided
        func_lower.arrow_this_var = captured_this;

        let entry = func_lower.create_block(name.clone().unwrap_or_else(|| "<fn>".to_string()));
        func_lower.builder.set_entry(entry);
        func_lower.builder.switch_to_block(entry);

        // Load arguments
        for (idx, param) in params.iter().enumerate() {
            let arg = func_lower.builder.load_arg(idx);
            func_lower
                .symbols
                .insert(param.name.to_string(), Variable::new(arg));
        }

        // First pass: collect nested function declarations for hoisting
        let mut hoisted_funcs: Vec<(String, Value)> = Vec::new();
        for stmt in &body.statements {
            if let Statement::FunctionDeclaration(nested_func) = stmt {
                let func_val = func_lower.collect_function_declaration(nested_func);
                if let Some((inner_name, val)) = func_val {
                    hoisted_funcs.push((inner_name, val));
                }
            }
        }
        // Assign hoisted nested functions to this function's symbol table
        for (inner_name, func_val) in &hoisted_funcs {
            let dst = func_lower.builder.alloc();
            func_lower.builder.assign(dst, *func_val);
            func_lower
                .symbols
                .insert(inner_name.clone(), Variable::new(dst));
        }

        // Register the function's own name for recursion
        if let Some(ref func_name) = name {
            let func_val = Value::Function(func_id);
            let dst = func_lower.builder.alloc();
            func_lower.builder.assign(dst, func_val);
            func_lower
                .symbols
                .insert(func_name.clone(), Variable::new(dst));
        }

        // Lower body statements (skip nested function declarations, already hoisted)
        if auto_return && body.statements.len() == 1 {
            // For arrow functions with expression body: () => expr
            // The body contains a single ExpressionStatement, return its value
            if let Statement::ExpressionStatement(expr_stmt) = &body.statements[0] {
                let result = func_lower.lower_expression(&expr_stmt.expression);
                func_lower.builder.return_(Some(result));
            } else {
                // Should not happen, but handle gracefully
                for stmt in &body.statements {
                    func_lower.lower_statement(stmt);
                }
                if !func_lower.current_block_is_terminated() {
                    func_lower.builder.return_(None);
                }
            }
        } else {
            for stmt in &body.statements {
                if matches!(stmt, Statement::FunctionDeclaration(_)) {
                    continue;
                }
                func_lower.lower_statement(stmt);
            }

            // Ensure function has a return
            if !func_lower.current_block_is_terminated() {
                func_lower.builder.return_(None);
            }
        }
        func_lower
            .builder
            .seal_block(func_lower.builder.current_block());

        self.builder.module_mut().define_function(func_id, func);

        // Restore builder to the current block (if one existed before)
        if let Some(block) = curr {
            self.builder.switch_to_block(block);
        }

        Value::Function(func_id)
    }

    // ──────────────────────── Helpers ────────────────────────

    fn lower_argument_expr(&mut self, arg: &Argument<'_>) -> Value {
        if let Argument::SpreadElement(spread) = arg {
            return self.lower_expression(&spread.argument);
        }
        // Argument inherits Expression variants; use as_expression()
        match arg.as_expression() {
            Some(expr) => self.lower_expression(expr),
            None => {
                log::warn!("unhandled argument type: {:?}", arg);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn lower_expression_or_null(&mut self, elem: &ArrayExpressionElement<'_>) -> Value {
        if let ArrayExpressionElement::SpreadElement(spread) = elem {
            return self.lower_expression(&spread.argument);
        }
        // ArrayExpressionElement inherits Expression variants; use as_expression()
        match elem.as_expression() {
            Some(expr) => self.lower_expression(expr),
            None => {
                log::warn!("unhandled array element: {:?}", elem);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn binding_pattern_name(&self, pat: &BindingPattern<'_>) -> String {
        match pat {
            BindingPattern::BindingIdentifier(id) => id.name.to_string(),
            _ => {
                log::warn!("destructuring patterns not yet supported");
                "<destructured>".to_string()
            }
        }
    }

    fn catch_clause_param_name(&self, catch: &CatchClause<'_>) -> String {
        match &catch.param {
            Some(param) => self.binding_pattern_name(&param.pattern),
            None => "<catch>".to_string(),
        }
    }

    fn property_key_to_string(&self, key: &PropertyKey<'_>) -> String {
        match key {
            PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            PropertyKey::StringLiteral(lit) => lit.value.to_string(),
            PropertyKey::NumericLiteral(lit) => lit.value.to_string(),
            _ => {
                log::warn!("computed property key not supported in object literal");
                "<computed>".to_string()
            }
        }
    }

    fn is_builtin_call(&self, expr: &Expression<'_>) -> bool {
        if let Expression::Identifier(ident) = expr {
            matches!(
                ident.name.as_str(),
                "Object"
                    | "Array"
                    | "Error"
                    | "TypeError"
                    | "ReferenceError"
                    | "RangeError"
                    | "Boolean"
                    | "Number"
                    | "String"
                    | "Symbol"
            )
        } else {
            false
        }
    }

    fn create_block(&mut self, label: impl Into<Name>) -> BlockId {
        self.builder.create_block(label.into())
    }

    fn loop_context(&self) -> &LoopContext {
        self.loop_contexts
            .last()
            .expect("break/continue outside loop")
    }

    fn enter_loop_context(&mut self, break_point: BlockId, continue_point: BlockId) {
        self.loop_contexts.push(LoopContext::new(
            break_point,
            continue_point,
            self.seh_depth,
        ));
    }

    fn leave_loop_context(&mut self) {
        self.loop_contexts.pop();
    }

    /// Check if the current block already has a terminator instruction.
    fn current_block_is_terminated(&self) -> bool {
        let cfg = self.builder.control_flow_graph();
        if let Some(block_id) = cfg.current_block() {
            if let Some(block) = cfg.get_block(block_id) {
                if let Some(last) = block.instructions().last() {
                    return last.is_terminator();
                }
            }
        }
        false
    }

    fn lower_block_like(&mut self, stmts: &[Statement<'_>]) {
        for stmt in stmts {
            self.lower_statement(stmt);
            if self.current_block_is_terminated() {
                break;
            }
        }
    }

    /// Like `lower_block_like` but returns the value of the last expression statement
    /// in the (linear) sequence, used to propagate block results as expression values.
    fn lower_block_like_with_result(&mut self, stmts: &[Statement<'_>]) -> Option<Value> {
        let mut last_result: Option<Value> = None;
        for stmt in stmts {
            if let Statement::ExpressionStatement(expr_stmt) = stmt {
                last_result = Some(self.lower_expression(&expr_stmt.expression));
            } else {
                self.lower_statement(stmt);
                last_result = None;
            }
            if self.current_block_is_terminated() {
                break;
            }
        }
        last_result
    }

    /// Recursively collect referenced and declared identifiers from a function body.
    /// `referenced` tracks names used in expressions; `declared` tracks names declared locally.
    fn collect_free_idents_from_body<'b>(
        &self,
        body: &FunctionBody<'b>,
        referenced: &mut std::collections::HashSet<&'b str>,
        declared: &mut std::collections::HashSet<String>,
    ) {
        for stmt in &body.statements {
            self.collect_free_idents_from_statement(stmt, referenced, declared);
        }
    }

    fn collect_free_idents_from_statement<'b>(
        &self,
        stmt: &Statement<'b>,
        referenced: &mut std::collections::HashSet<&'b str>,
        declared: &mut std::collections::HashSet<String>,
    ) {
        match stmt {
            Statement::ExpressionStatement(expr_stmt) => {
                self.collect_free_idents_from_expression(
                    &expr_stmt.expression,
                    referenced,
                    declared,
                );
            }
            Statement::ReturnStatement(ret) => {
                if let Some(arg) = &ret.argument {
                    self.collect_free_idents_from_expression(arg, referenced, declared);
                }
            }
            Statement::VariableDeclaration(decl) => {
                for d in &decl.declarations {
                    let name = self.binding_pattern_name(&d.id);
                    declared.insert(name);
                    if let Some(init) = &d.init {
                        self.collect_free_idents_from_expression(init, referenced, declared);
                    }
                }
            }
            Statement::BlockStatement(block) => {
                for s in &block.body {
                    self.collect_free_idents_from_statement(s, referenced, declared);
                }
            }
            Statement::IfStatement(if_stmt) => {
                self.collect_free_idents_from_expression(&if_stmt.test, referenced, declared);
                self.collect_free_idents_from_statement(&if_stmt.consequent, referenced, declared);
                if let Some(alt) = &if_stmt.alternate {
                    self.collect_free_idents_from_statement(alt, referenced, declared);
                }
            }
            Statement::ForStatement(for_stmt) => {
                if let Some(init) = &for_stmt.init {
                    match init {
                        ForStatementInit::VariableDeclaration(decl) => {
                            for d in &decl.declarations {
                                let name = self.binding_pattern_name(&d.id);
                                declared.insert(name);
                                if let Some(init) = &d.init {
                                    self.collect_free_idents_from_expression(
                                        init, referenced, declared,
                                    );
                                }
                            }
                        }
                        _ => {
                            if let Some(expr) = init.as_expression() {
                                self.collect_free_idents_from_expression(
                                    expr, referenced, declared,
                                );
                            }
                        }
                    }
                }
                if let Some(test) = &for_stmt.test {
                    self.collect_free_idents_from_expression(test, referenced, declared);
                }
                if let Some(update) = &for_stmt.update {
                    self.collect_free_idents_from_expression(update, referenced, declared);
                }
                self.collect_free_idents_from_statement(&for_stmt.body, referenced, declared);
            }
            Statement::WhileStatement(while_stmt) => {
                self.collect_free_idents_from_expression(&while_stmt.test, referenced, declared);
                self.collect_free_idents_from_statement(&while_stmt.body, referenced, declared);
            }
            Statement::ThrowStatement(throw) => {
                self.collect_free_idents_from_expression(&throw.argument, referenced, declared);
            }
            Statement::TryStatement(try_stmt) => {
                for s in &try_stmt.block.body {
                    self.collect_free_idents_from_statement(s, referenced, declared);
                }
                if let Some(handler) = &try_stmt.handler {
                    for s in &handler.body.body {
                        self.collect_free_idents_from_statement(s, referenced, declared);
                    }
                }
                if let Some(finalizer) = &try_stmt.finalizer {
                    for s in &finalizer.body {
                        self.collect_free_idents_from_statement(s, referenced, declared);
                    }
                }
            }
            Statement::SwitchStatement(switch) => {
                self.collect_free_idents_from_expression(
                    &switch.discriminant,
                    referenced,
                    declared,
                );
                for case in &switch.cases {
                    if let Some(test) = &case.test {
                        self.collect_free_idents_from_expression(test, referenced, declared);
                    }
                    for s in &case.consequent {
                        self.collect_free_idents_from_statement(s, referenced, declared);
                    }
                }
            }
            Statement::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    declared.insert(id.name.to_string());
                }
            }
            _ => {}
        }
    }

    fn collect_free_idents_from_expression<'b>(
        &self,
        expr: &Expression<'b>,
        referenced: &mut std::collections::HashSet<&'b str>,
        declared: &mut std::collections::HashSet<String>,
    ) {
        match expr {
            Expression::Identifier(ident) => match ident.name.as_str() {
                "undefined" | "NaN" | "Infinity" | "this" => {}
                _ => {
                    referenced.insert(ident.name.as_str());
                }
            },
            Expression::BinaryExpression(bin) => {
                self.collect_free_idents_from_expression(&bin.left, referenced, declared);
                self.collect_free_idents_from_expression(&bin.right, referenced, declared);
            }
            Expression::UnaryExpression(unary) => {
                self.collect_free_idents_from_expression(&unary.argument, referenced, declared);
            }
            Expression::LogicalExpression(logical) => {
                self.collect_free_idents_from_expression(&logical.left, referenced, declared);
                self.collect_free_idents_from_expression(&logical.right, referenced, declared);
            }
            Expression::AssignmentExpression(assign) => {
                match &assign.left {
                    AssignmentTarget::AssignmentTargetIdentifier(ident) => {
                        referenced.insert(ident.name.as_str());
                    }
                    _ => {}
                }
                self.collect_free_idents_from_expression(&assign.right, referenced, declared);
            }
            Expression::CallExpression(call) => {
                self.collect_free_idents_from_expression(&call.callee, referenced, declared);
                for arg in &call.arguments {
                    if let Some(expr) = arg.as_expression() {
                        self.collect_free_idents_from_expression(expr, referenced, declared);
                    }
                }
            }
            Expression::ConditionalExpression(cond) => {
                self.collect_free_idents_from_expression(&cond.test, referenced, declared);
                self.collect_free_idents_from_expression(&cond.consequent, referenced, declared);
                self.collect_free_idents_from_expression(&cond.alternate, referenced, declared);
            }
            Expression::StaticMemberExpression(member) => {
                self.collect_free_idents_from_expression(&member.object, referenced, declared);
            }
            Expression::ComputedMemberExpression(member) => {
                self.collect_free_idents_from_expression(&member.object, referenced, declared);
                self.collect_free_idents_from_expression(&member.expression, referenced, declared);
            }
            Expression::PrivateFieldExpression(member) => {
                self.collect_free_idents_from_expression(&member.object, referenced, declared);
            }
            Expression::SequenceExpression(seq) => {
                for e in &seq.expressions {
                    self.collect_free_idents_from_expression(e, referenced, declared);
                }
            }
            Expression::ParenthesizedExpression(paren) => {
                self.collect_free_idents_from_expression(&paren.expression, referenced, declared);
            }
            Expression::TemplateLiteral(tpl) => {
                for exp in &tpl.expressions {
                    self.collect_free_idents_from_expression(exp, referenced, declared);
                }
            }
            Expression::UpdateExpression(update) => match &update.argument {
                SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
                    referenced.insert(ident.name.as_str());
                }
                _ => {}
            },
            Expression::ArrayExpression(arr) => {
                for elem in &arr.elements {
                    if let Some(expr) = elem.as_expression() {
                        self.collect_free_idents_from_expression(expr, referenced, declared);
                    }
                }
            }
            Expression::ObjectExpression(obj) => {
                for prop in &obj.properties {
                    if let ObjectPropertyKind::ObjectProperty(p) = prop {
                        self.collect_free_idents_from_expression(&p.value, referenced, declared);
                    }
                }
            }
            Expression::NewExpression(new) => {
                self.collect_free_idents_from_expression(&new.callee, referenced, declared);
                for arg in &new.arguments {
                    if let Some(expr) = arg.as_expression() {
                        self.collect_free_idents_from_expression(expr, referenced, declared);
                    }
                }
            }
            Expression::ArrowFunctionExpression(arrow) => {
                // Recurse into nested arrow bodies so outer arrows can discover
                // variables that need capturing (e.g. `(y) => (z) => x + y + z`)
                // Also mark the inner arrow's params as declared
                for param in &arrow.params.items {
                    let name = self.binding_pattern_name(&param.pattern);
                    declared.insert(name);
                }
                for stmt in &arrow.body.statements {
                    self.collect_free_idents_from_statement(stmt, referenced, declared);
                }
            }
            Expression::FunctionExpression(_) => {
                // Don't recurse into regular function expressions
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Compiler;
    use crate::vm::{VM, Value};

    /// Helper: compile and run JS code, return the result
    fn eval(js: &str) -> Value {
        let mut compiler = Compiler::new();
        let module = compiler.compile(js).expect("compilation failed");
        let mut vm = VM::new();
        vm.run(&module).expect("runtime error")
    }

    // ──────────────────────── Compilation Pipeline Tests ────────────────────────

    #[test]
    fn test_compile_number_literal() {
        assert_eq!(eval("42;"), Value::Number(42.0));
    }

    #[test]
    fn test_compile_string_literal_directive() {
        // Test that standalone string literals work via directive handling
        assert_eq!(eval("\"hello\";"), Value::string("hello"));
    }

    #[test]
    fn test_compile_string_in_variable() {
        assert_eq!(eval("let x = \"world\"; x;"), Value::string("world"));
    }

    #[test]
    fn test_compile_arithmetic() {
        assert_eq!(eval("2 + 3 * 4;"), Value::Number(14.0));
    }

    #[test]
    fn test_compile_comparison() {
        assert_eq!(eval("5 > 3;"), Value::Bool(true));
        assert_eq!(eval("3 > 5;"), Value::Bool(false));
        assert_eq!(eval("5 == 5;"), Value::Bool(true));
        assert_eq!(eval("5 != 3;"), Value::Bool(true));
    }

    #[test]
    fn test_compile_logical_operators() {
        assert_eq!(eval("let x = true && true; x;"), Value::Bool(true));
        assert_eq!(eval("let x = true && false; x;"), Value::Bool(false));
        assert_eq!(eval("let x = false || true; x;"), Value::Bool(true));
        assert_eq!(eval("let x = false || false; x;"), Value::Bool(false));
    }

    #[test]
    fn test_compile_typeof() {
        assert_eq!(eval("typeof 42;"), Value::string("number"));
        assert_eq!(eval("typeof true;"), Value::string("boolean"));
        assert_eq!(eval("typeof undefined;"), Value::string("undefined"));
        assert_eq!(eval("typeof null;"), Value::string("object"));
        assert_eq!(eval("typeof \"hello\";"), Value::string("string"));
    }

    #[test]
    fn test_compile_unary_operators() {
        assert_eq!(eval("-5;"), Value::Number(-5.0));
        assert_eq!(eval("!true;"), Value::Bool(false));
        assert_eq!(eval("!false;"), Value::Bool(true));
        assert_eq!(eval("void 0;"), Value::Undefined);
    }

    #[test]
    fn test_compile_variable_declaration_and_use() {
        assert_eq!(eval("let x = 10; let y = 20; x + y;"), Value::Number(30.0));
    }

    #[test]
    fn test_compile_variable_assignment() {
        assert_eq!(eval("let x = 1; x = 42; x;"), Value::Number(42.0));
    }

    #[test]
    fn test_compile_compound_assignment() {
        assert_eq!(eval("let x = 10; x += 5; x;"), Value::Number(15.0));
        assert_eq!(eval("let x = 10; x -= 3; x;"), Value::Number(7.0));
        assert_eq!(eval("let x = 10; x *= 2; x;"), Value::Number(20.0));
        assert_eq!(eval("let x = 10; x /= 2; x;"), Value::Number(5.0));
        assert_eq!(eval("let x = 10; x %= 3; x;"), Value::Number(1.0));
    }

    #[test]
    fn test_compile_if_else() {
        // Note: if/else blocks don't return values directly in current implementation
        // Use variable assignment to capture results
        assert_eq!(
            eval("let x; if (true) { x = 1; } else { x = 2; } x;"),
            Value::Number(1.0)
        );
        assert_eq!(
            eval("let x; if (false) { x = 1; } else { x = 2; } x;"),
            Value::Number(2.0)
        );
    }

    #[test]
    fn test_compile_if_without_else() {
        assert_eq!(
            eval("let x = 0; if (true) { x = 42; } x;"),
            Value::Number(42.0)
        );
    }

    #[test]
    fn test_compile_while_loop() {
        assert_eq!(
            eval("let i = 0; let sum = 0; while (i < 5) { sum += i; i++; } sum;"),
            Value::Number(10.0)
        );
    }

    #[test]
    fn test_compile_for_loop() {
        assert_eq!(
            eval("let sum = 0; for (let i = 0; i < 5; i++) { sum += i; } sum;"),
            Value::Number(10.0)
        );
    }

    #[test]
    fn test_compile_break_continue() {
        // break
        assert_eq!(
            eval("let i = 0; while (true) { if (i == 3) break; i++; } i;"),
            Value::Number(3.0)
        );
        // continue
        assert_eq!(
            eval(
                "let sum = 0; for (let i = 0; i < 5; i++) { if (i == 2) continue; sum += i; } sum;"
            ),
            Value::Number(8.0)
        );
    }

    #[test]
    fn test_compile_nested_blocks() {
        assert_eq!(eval("let x = 1; { let x = 2; } x;"), Value::Number(1.0));
    }

    #[test]
    fn test_compile_conditional_expression() {
        assert_eq!(eval("true ? 1 : 2;"), Value::Number(1.0));
        assert_eq!(eval("false ? 1 : 2;"), Value::Number(2.0));
    }

    #[test]
    fn test_compile_sequence_expression() {
        assert_eq!(eval("(1, 2, 3);"), Value::Number(3.0));
    }

    #[test]
    fn test_compile_null_literal() {
        assert_eq!(eval("null;"), Value::Null);
    }

    #[test]
    fn test_compile_boolean_literals() {
        assert_eq!(eval("true;"), Value::Bool(true));
        assert_eq!(eval("false;"), Value::Bool(false));
    }

    #[test]
    fn test_compile_undefined() {
        assert_eq!(eval("undefined;"), Value::Undefined);
    }

    #[test]
    fn test_compile_parenthesized_expression() {
        assert_eq!(eval("(2 + 3) * 4;"), Value::Number(20.0));
    }

    #[test]
    fn test_compile_chained_expressions() {
        assert_eq!(eval("1 + 2 + 3 + 4;"), Value::Number(10.0));
    }

    #[test]
    fn test_compile_precedence() {
        // * has higher precedence than +
        assert_eq!(eval("2 + 3 * 4;"), Value::Number(14.0));
        // parentheses override precedence
        assert_eq!(eval("(2 + 3) * 4;"), Value::Number(20.0));
    }

    #[test]
    fn test_compile_string_concatenation() {
        assert_eq!(
            eval("\"hello\" + \" world\";"),
            Value::string("hello world")
        );
    }

    #[test]
    fn test_compile_string_number_coercion() {
        assert_eq!(
            eval("\"The answer is \" + 42;"),
            Value::string("The answer is 42")
        );
    }
}
