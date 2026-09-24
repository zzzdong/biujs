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

/// State for one open `try` statement, used to model the control-flow edges
/// that a delayed `break`/`continue` creates through its `finally` block.
struct SehFrameInfo {
    /// The `finally` block of this try statement, if it has one.
    finally_blk: Option<BlockId>,
    /// Trampoline blocks that will be entered once this frame's `finally` has
    /// run. Each gets a CFG edge from `finally_blk` so that liveness/SSA see the
    /// real data flow (the finally block is what produces the values the
    /// trampoline observes).
    pending_exits: Vec<BlockId>,
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

/// The name of an object-literal property or class member.
///
/// Non-computed literals stay static strings (cheaper, and the property order
/// is unchanged); a computed key such as `[expr]` becomes a runtime value that
/// `ToPropertyKey` converts when the member is defined.
enum MemberKey {
    Static(String),
    Dynamic(Value),
}

/// Debug name for a class member function (`C.m`, `C.[computed]`).
fn member_display_name(class_name: &str, key: &MemberKey) -> String {
    match key {
        MemberKey::Static(name) => format!("{class_name}.{name}"),
        MemberKey::Dynamic(_) => format!("{class_name}.[computed]"),
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
    /// True only for the lowerer of the script's top-level `main` function.
    ///
    /// Script-level bindings are also published into the *global environment*
    /// (`StoreEnv`) so that nested functions — which get their own frame and
    /// cannot read the caller's registers — can still resolve them via
    /// `LoadEnv`. This is what makes closures over script scope work.
    is_script: bool,
    /// Nesting depth of block/catch scopes; 0 means "directly in script scope".
    scope_depth: usize,
    /// One entry per open `try` statement (parallel to `seh_depth`).
    seh_frames: Vec<SehFrameInfo>,
    /// Names published into the global environment by this (script) lowerer.
    global_names: std::collections::HashSet<String>,
}

impl<'a> JSASTLower<'a> {
    pub fn new(builder: &'a mut dyn InstBuilder, symbols: SymbolTable<Variable>) -> Self {
        Self {
            builder,
            symbols,
            loop_contexts: Vec::new(),
            seh_depth: 0,
            arrow_this_var: None,
            is_script: false,
            scope_depth: 0,
            seh_frames: Vec::new(),
            global_names: std::collections::HashSet::new(),
        }
    }

    /// Create the lowerer for the script's top level.
    pub fn new_script(builder: &'a mut dyn InstBuilder, symbols: SymbolTable<Variable>) -> Self {
        let mut lower = Self::new(builder, symbols);
        lower.is_script = true;
        lower
    }

    /// Whether a binding declared right here becomes a script-level global.
    fn at_script_scope(&self) -> bool {
        self.is_script && self.scope_depth == 0
    }

    /// Publish `name -> value` into the global environment (script scope only).
    fn define_global(&mut self, name: &str, value: Value) {
        self.builder.store_external_variable(name.to_string(), value);
        self.global_names.insert(name.to_string());
    }

    /// Re-publish a global after it has been reassigned.
    fn sync_global(&mut self, name: &str, value: Value) {
        if self.global_names.contains(name) {
            self.builder
                .store_external_variable(name.to_string(), value);
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

        // Now assign hoisted functions (we have a current block)
        for (name, func_val) in hoisted_funcs {
            let dst = self.builder.alloc();
            self.builder.assign(dst, func_val);
            // Script-level declarations are also published to the global
            // environment so nested functions (which have their own frame and
            // cannot read the caller's registers) can resolve them via LoadEnv.
            if self.at_script_scope() {
                self.define_global(&name, func_val);
            }
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
            Statement::DoWhileStatement(do_while) => self.lower_do_while(do_while),
            Statement::ForStatement(for_stmt) => self.lower_for(for_stmt),
            Statement::ForOfStatement(for_of) => self.lower_for_of(for_of),
            Statement::ForInStatement(for_in) => self.lower_for_in(for_in),
            Statement::SwitchStatement(switch) => self.lower_switch(switch),
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
            if self.at_script_scope() {
                self.define_global(&id.name.to_string(), class_val);
            }
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

        /// One member of a class body.
        enum Member<'a> {
            Constructor(&'a Function<'a>),
            Method {
                key: MemberKey,
                func: &'a Function<'a>,
            },
            Accessor {
                key: MemberKey,
                is_get: bool,
                func: &'a Function<'a>,
            },
            StaticMethod {
                key: MemberKey,
                func: &'a Function<'a>,
            },
            StaticAccessor {
                key: MemberKey,
                is_get: bool,
                func: &'a Function<'a>,
            },
            StaticField {
                key: MemberKey,
                field: &'a PropertyDefinition<'a>,
            },
        }

        // 1. Walk the body in source order. Computed keys are evaluated right
        //    here — once, in order — exactly as ClassDefinitionEvaluation does.
        let mut members: Vec<Member<'_>> = Vec::new();
        let mut instance_fields: Vec<&PropertyDefinition<'_>> = Vec::new();

        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(property_def) => {
                    if property_def.r#static {
                        let key =
                            self.lower_member_key(&property_def.key, property_def.computed);
                        members.push(Member::StaticField {
                            key,
                            field: property_def,
                        });
                    } else {
                        // Instance fields are installed by the constructor, which
                        // evaluates their keys itself.
                        instance_fields.push(property_def);
                    }
                }
                ClassElement::MethodDefinition(method_def) => {
                    if method_def.kind == MethodDefinitionKind::Constructor {
                        members.push(Member::Constructor(method_def.value.as_ref()));
                        continue;
                    }
                    let is_accessor = matches!(
                        method_def.kind,
                        MethodDefinitionKind::Get | MethodDefinitionKind::Set
                    );
                    let is_get = method_def.kind == MethodDefinitionKind::Get;
                    let key = self.lower_member_key(&method_def.key, method_def.computed);
                    let func = method_def.value.as_ref();
                    members.push(if method_def.r#static {
                        if is_accessor {
                            Member::StaticAccessor { key, is_get, func }
                        } else {
                            Member::StaticMethod { key, func }
                        }
                    } else if is_accessor {
                        Member::Accessor { key, is_get, func }
                    } else {
                        Member::Method { key, func }
                    });
                }
                _ => {}
            }
        }

        // 2. Create the prototype object.
        //
        // With `extends P`, the prototype must inherit from `P.prototype`;
        // without it, a fresh object is enough.
        let mut super_ctor: Option<Value> = None;
        // A constructor of a class with `extends` is "derived": `this` starts
        // uninitialized and only `super()` binds it (ES 9.2.2).
        let has_heritage = class.super_class.is_some();
        let proto = match &class.super_class {
            Some(super_expr) => {
                let parent = self.lower_expression(super_expr);
                super_ctor = Some(parent);
                let parent_proto = self.builder.get_property(parent, "prototype");
                let object_fn = self.builder.load_external_variable("Object".to_string());
                self.builder
                    .call_property(object_fn, "create", vec![parent_proto])
            }
            None => self.builder.make_object(),
        };

        // 3. Build the constructor function (only a non-computed `constructor`
        //    member counts as one; `['constructor']() {}` is an ordinary
        //    prototype method).
        let mut constructor_id = None;
        for member in &members {
            if let Member::Constructor(func) = member {
                // `C.name` is the class name (ES 14.5.14 ClassDefinitionEvaluation
                // sets the constructor's name to the binding identifier).
                constructor_id = Some(self.lower_function_inner(
                    Some(class_name.clone()),
                    &func.params.items,
                    func.body.as_ref().unwrap(),
                    None,
                    false,
                    &[],
                    false,
                    &instance_fields,
                    false,
                    has_heritage,));
            }
        }

        // 4. If no constructor, create a default one
        let constructor_id = constructor_id.unwrap_or_else(|| {
            let func_sig = FuncSignature::new(class_name.clone(), vec![]);
            // The default constructor of a derived class forwards every
            // argument: `constructor(...args) { super(...args); }` (ES 14.5.15).
            let func_sig = if has_heritage {
                func_sig.as_derived_ctor()
            } else {
                func_sig
            };
            let func_id = self.builder.module_mut().declare_function(func_sig.clone());
            let mut func = IrFunction::new(func_id, func_sig);
            let symbols = self.symbols.clone();
            let mut func_builder = FunctionBuilder::new(self.builder.module_mut(), &mut func);
            let mut func_lower = JSASTLower::new(&mut func_builder, symbols);
            let entry = func_lower.create_block("default_ctor");
            func_lower.builder.set_entry(entry);
            func_lower.builder.switch_to_block(entry);
            if has_heritage {
                let parent = func_lower.lower_super_ctor();
                let argv = func_lower.builder.make_rest(0);
                func_lower
                    .builder
                    .call_super(parent, Value::Primitive(Primitive::Undefined), argv);
            }
            func_lower.builder.return_(None);
            func_lower
                .builder
                .seal_block(func_lower.builder.current_block());
            self.builder.module_mut().define_function(func_id, func);
            Value::Function(func_id)
        });

        // 5. Wrap the constructor in a FunctionObject and run MakeConstructor.
        //    `prototype` (and the `constructor` back-reference it installs on
        //    the prototype) exist *before* the body members, which is why a
        //    computed `['constructor']` member can overwrite the back-reference
        //    and why `prototype` comes first in property order.
        let func_obj = self.builder.make_func_obj(constructor_id);
        let proto_desc = self.data_descriptor(proto, true, false, false);
        self.define_property_named(func_obj, "prototype", proto_desc);
        let ctor_desc = self.data_descriptor(func_obj, true, false, true);
        self.define_property_named(proto, "constructor", ctor_desc);

        // Marks a class constructor: calling one without `new` is a TypeError.
        let flag_desc = self.data_descriptor(
            Value::Primitive(Primitive::Boolean(true)),
            false,
            false,
            false,
        );
        self.define_property_named(func_obj, crate::builtins::CLASS_CTOR_FLAG, flag_desc);

        // 5b. Inheritance wiring. The parent constructor is published as
        //     `__super__` on the child constructor: methods are compiled as
        //     separate functions and cannot reference the enclosing frame's
        //     registers, so `super(...)` resolves the parent at run time from
        //     `Object.getPrototypeOf(this).constructor.__super__`.
        if let Some(parent) = super_ctor {
            let super_desc = self.data_descriptor(parent, true, false, true);
            self.define_property_named(func_obj, "__super__", super_desc);
            // Static inheritance: `Object.setPrototypeOf(child, parent)`.
            let object_fn = self.builder.load_external_variable("Object".to_string());
            self.builder
                .call_property(object_fn, "setPrototypeOf", vec![func_obj, parent]);
        }

        // 5c. `[[HomeObject]]` wiring for `super.prop` / `super.m()`:
        //     instance members look on `getPrototypeOf(C.prototype)`, static
        //     members on `getPrototypeOf(C)`. Both are fixed here, at class
        //     definition time.
        let instance_super_proto = self.get_prototype_of(proto);
        let static_super_proto = self.get_prototype_of(func_obj);
        let func_obj = self.attach_super_proto(func_obj, instance_super_proto.clone());

        // 6. Install the body members in source order: prototype members on the
        //    prototype object, static members on the constructor itself.
        for member in &members {
            match member {
                Member::Constructor(_) => {}
                Member::Method { key, func } => {
                    let name = member_display_name(&class_name, key);
                    let func_val = self.lower_function_inner(
                        Some(name),
                        &func.params.items,
                        func.body.as_ref().unwrap(),
                        None,
                        false,
                        &[],
                        false,
                        &[],
                        func.generator,
                        false,);
                    let func_val = self.attach_super_proto(func_val, instance_super_proto.clone());
                    let desc = self.method_descriptor(func_val);
                    self.define_member(proto, key, desc);
                }
                Member::StaticMethod { key, func } => {
                    let name = member_display_name(&class_name, key);
                    let func_val = self.lower_function_inner(
                        Some(name),
                        &func.params.items,
                        func.body.as_ref().unwrap(),
                        None,
                        false,
                        &[],
                        false,
                        &[],
                        func.generator,
                        false,);
                    let func_val = self.attach_super_proto(func_val, static_super_proto.clone());
                    let desc = self.method_descriptor(func_val);
                    self.define_member(func_obj, key, desc);
                }
                Member::Accessor { key, is_get, func } => {
                    let name = member_display_name(&class_name, key);
                    let func_val = self.lower_function_inner(
                        Some(name),
                        &func.params.items,
                        func.body.as_ref().unwrap(),
                        None,
                        false,
                        &[],
                        false,
                        &[],
                        func.generator,
                        false,);
                    let func_val = self.attach_super_proto(func_val, instance_super_proto.clone());
                    let (getter, setter) = if *is_get {
                        (Some(func_val), None)
                    } else {
                        (None, Some(func_val))
                    };
                    let desc = self.accessor_descriptor(getter, setter, false);
                    self.define_member(proto, key, desc);
                }
                Member::StaticAccessor { key, is_get, func } => {
                    let name = member_display_name(&class_name, key);
                    let func_val = self.lower_function_inner(
                        Some(name),
                        &func.params.items,
                        func.body.as_ref().unwrap(),
                        None,
                        false,
                        &[],
                        false,
                        &[],
                        func.generator,
                        false,);
                    let func_val = self.attach_super_proto(func_val, static_super_proto.clone());
                    let (getter, setter) = if *is_get {
                        (Some(func_val), None)
                    } else {
                        (None, Some(func_val))
                    };
                    let desc = self.accessor_descriptor(getter, setter, false);
                    self.define_member(func_obj, key, desc);
                }
                // Static fields are ordinary data properties, evaluated once.
                Member::StaticField { key, field } => {
                    let value = match &field.value {
                        Some(init) => self.lower_expression(init),
                        None => Value::Primitive(Primitive::Undefined),
                    };
                    self.set_member(func_obj, key, value);
                }
            }
        }

        func_obj
    }

    /// `[[Value]]` descriptor with explicit attributes.
    fn data_descriptor(
        &mut self,
        value: Value,
        writable: bool,
        enumerable: bool,
        configurable: bool,
    ) -> Value {
        let desc = self.builder.make_object();
        self.builder.set_property(desc, "value", value);
        self.builder.set_property(
            desc,
            "writable",
            Value::Primitive(Primitive::Boolean(writable)),
        );
        self.builder.set_property(
            desc,
            "enumerable",
            Value::Primitive(Primitive::Boolean(enumerable)),
        );
        self.builder.set_property(
            desc,
            "configurable",
            Value::Primitive(Primitive::Boolean(configurable)),
        );
        desc
    }

    /// `Object.defineProperty(target, name, desc)` for a static name.
    fn define_property_named(&mut self, target: Value, name: &str, desc: Value) {
        let key = MemberKey::Static(name.to_string());
        self.define_member(target, &key, desc);
    }

    /// Record the prototype of a member's `[[HomeObject]]` on its function.
    ///
    /// `super.x` looks the property up on `getPrototypeOf(homeObject)`, and the
    /// home object is fixed when the class (or object literal) is defined. The
    /// value travels on the function object so that `super` stays correct at any
    /// inheritance depth — and so arrow functions can inherit it at creation
    /// time instead of re-deriving it from the receiver.
    fn attach_super_proto(&mut self, func_val: Value, super_proto: Value) -> Value {
        let boxed = match func_val {
            Value::Function(_) => self.builder.make_func_obj(func_val),
            other => other,
        };
        let desc = self.data_descriptor(super_proto, true, false, true);
        self.define_property_named(boxed, "__superProto__", desc);
        boxed
    }

    /// `Object.getPrototypeOf(value)` as an IR value.
    fn get_prototype_of(&mut self, value: Value) -> Value {
        let object_fn = self.builder.load_external_variable("Object".to_string());
        self.builder
            .call_property(object_fn, "getPrototypeOf", vec![value])
    }

    fn lower_variable_declaration(&mut self, decl: &VariableDeclaration<'_>) {
        for declarator in &decl.declarations {
            let name = self.binding_pattern_name(&declarator.id);

            let value = match &declarator.init {
                Some(init) => Some(self.lower_expression(init)),
                None => None,
            };

            if let BindingPattern::BindingIdentifier(_) = &declarator.id {
                let dst = self.builder.alloc();
                if let Some(value) = value {
                    self.builder.assign(dst, value);
                    if self.at_script_scope() {
                        self.define_global(&name, value);
                    }
                } else if self.at_script_scope() {
                    // `let x;` — publish the (undefined) binding so nested
                    // reads resolve to undefined instead of ReferenceError.
                    self.define_global(&name, Value::Primitive(Primitive::Undefined));
                }
                // else: dst stays as default (undefined)
                self.symbols.insert(name, Variable::new(dst));
            } else if let Some(value) = value {
                // Destructuring declaration: `[a, b] = value` / `{x} = value`.
                self.bind_pattern(&declarator.id, value, true);
            } else {
                log::warn!("destructuring declaration without initializer");
            }
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

    /// `for (lhs of rhs) body` — iterate `rhs` via the iterator protocol.
    ///
    /// CFG (see docs/m1-syntax-plan.md §3.2):
    /// ```text
    /// iter_blk: it = MakeIterator(rhs)          ← rhs evaluated once
    /// cond:     (item, has_next) = IterateNext(it)
    /// body:     bind lhs = item; body; → cond   ← continue target
    /// close:    IteratorClose(it) → after       ← break target
    /// ```
    /// `yield* expr` (ES 14.4.14) desugared to `next()` + `yield`.
    ///
    /// ```text
    /// iter = GetIterator(expr); sent = undefined
    /// cond: r = iter.next(sent)
    ///       if (r.done) → done
    /// body: sent = yield r.value → cond
    /// done: result = r.value; IteratorClose(iter) → after
    /// ```
    ///
    /// The desugaring is what makes the sent value reach the delegate: it is
    /// the value of the `yield` inside the loop, so `next(v)` supplies exactly
    /// the argument the inner iterator's `next` should see.
    fn lower_yield_delegate(&mut self, expr: &Expression<'_>) -> Value {
        let src = self.lower_expression(expr);
        let iter = self.builder.make_iterator(src);
        // The iterator is pending until the delegation finishes: a `return()`
        // or `throw()` on the generator has to close it.
        self.builder.delegate_open(iter);

        let sent = self.builder.alloc();
        self.builder
            .assign(sent, Value::Primitive(Primitive::Undefined));
        let result = self.builder.alloc();
        self.builder
            .assign(result, Value::Primitive(Primitive::Undefined));

        let cond_blk = self.create_block("delegate_cond");
        let body_blk = self.create_block("delegate_body");
        let done_blk = self.create_block("delegate_done");
        let after_blk = self.create_block("delegate_after");

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let step = self.builder.call_property(iter, "next", vec![sent]);
        let is_done = self.builder.get_property(step, "done");
        self.builder.br_if(is_done, done_blk, body_blk);

        self.builder.switch_to_block(body_blk);
        let value = self.builder.get_property(step, "value");
        let next_sent = self.builder.yield_(Some(value));
        self.builder.assign(sent, next_sent);
        self.builder.jump(cond_blk);

        self.builder.switch_to_block(done_blk);
        let returned = self.builder.get_property(step, "value");
        self.builder.assign(result, returned);
        self.builder.delegate_close(iter);
        self.builder.iterator_close(iter);
        self.builder.jump(after_blk);

        self.builder.switch_to_block(after_blk);
        result
    }

    fn lower_for_of(&mut self, for_of: &ForOfStatement<'_>) {
        if for_of.r#await {
            log::warn!("for-await not supported; treating as for-of");
        }

        let src = self.lower_expression(&for_of.right);
        let it = self.builder.make_iterator(src);

        let cond_blk = self.create_block("forof_cond");
        let body_blk = self.create_block("forof_body");
        let close_blk = self.create_block("forof_close");
        let after_blk = self.create_block("forof_after");

        // break must pass through IteratorClose; continue goes to cond.
        self.enter_loop_context(close_blk, cond_blk);

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let (item, has_next) = self.builder.iterate_next(it);
        self.builder.br_if(has_next, body_blk, close_blk);

        self.builder.switch_to_block(body_blk);
        self.bind_for_of_left(&for_of.left, item);
        self.lower_statement(&for_of.body);
        if !self.current_block_is_terminated() {
            self.builder.jump(cond_blk);
        }

        self.leave_loop_context();
        self.builder.switch_to_block(close_blk);
        self.builder.iterator_close(it);
        self.builder.jump(after_blk);
        self.builder.switch_to_block(after_blk);
    }

    /// `for (lhs in rhs) body` — desugared to for-of over `Object.keys(rhs)`.
    fn lower_for_in(&mut self, for_in: &ForInStatement<'_>) {
        let obj = self.lower_expression(&for_in.right);
        let object_fn = self.builder.load_external_variable("Object".to_string());
        let keys = self
            .builder
            .call_property(object_fn, "keys", vec![obj]);

        // Reuse the for-of CFG with the key array as the source.
        let it = self.builder.make_iterator(keys);

        let cond_blk = self.create_block("forin_cond");
        let body_blk = self.create_block("forin_body");
        let close_blk = self.create_block("forin_close");
        let after_blk = self.create_block("forin_after");

        self.enter_loop_context(close_blk, cond_blk);

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let (item, has_next) = self.builder.iterate_next(it);
        self.builder.br_if(has_next, body_blk, close_blk);

        self.builder.switch_to_block(body_blk);
        self.bind_for_of_left(&for_in.left, item);
        self.lower_statement(&for_in.body);
        if !self.current_block_is_terminated() {
            self.builder.jump(cond_blk);
        }

        self.leave_loop_context();
        self.builder.switch_to_block(close_blk);
        self.builder.iterator_close(it);
        self.builder.jump(after_blk);
        self.builder.switch_to_block(after_blk);
    }

    /// Bind the `left` of a for-of/for-in head to `item`.
    fn bind_for_of_left(&mut self, left: &ForStatementLeft<'_>, item: Value) {
        match left {
            ForStatementLeft::VariableDeclaration(decl) => {
                for declarator in &decl.declarations {
                    // Destructuring patterns land here in T7; for now bind the
                    // simple identifier.
                    let name = self.binding_pattern_name(&declarator.id);
                    let dst = self.builder.alloc();
                    self.builder.assign(dst, item);
                    self.symbols.insert(name, Variable::new(dst));
                }
            }
            ForStatementLeft::AssignmentTargetIdentifier(ident) => {
                match self.symbols.lookup(ident.name.as_str()) {
                    Some(var) => {
                        self.builder.assign(var.0, item);
                        self.sync_global(ident.name.as_str(), item);
                    }
                    None => {
                        self.builder
                            .store_external_variable(ident.name.to_string(), item);
                    }
                }
            }
            ForStatementLeft::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                self.builder.index_set(object, index, item);
            }
            ForStatementLeft::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                self.builder
                    .set_property(object, member.property.name.as_str(), item);
            }
            _ => {
                log::warn!("unimplemented for-of/for-in assignment target");
            }
        }
    }

    /// `do { body } while (test);` — the body always runs at least once.
    fn lower_do_while(&mut self, do_while: &DoWhileStatement<'_>) {
        let body_blk = self.create_block("dowhile_body");
        let cond_blk = self.create_block("dowhile_cond");
        let after_blk = self.create_block("dowhile_after");

        self.enter_loop_context(after_blk, cond_blk);

        self.builder.jump(body_blk);
        self.builder.switch_to_block(body_blk);
        self.lower_statement(&do_while.body);
        if !self.current_block_is_terminated() {
            self.builder.jump(cond_blk);
        }

        self.builder.switch_to_block(cond_blk);
        let cond = self.lower_expression(&do_while.test);
        self.builder.br_if(cond, body_blk, after_blk);

        self.leave_loop_context();
        self.builder.switch_to_block(after_blk);
    }

    /// `switch (disc) { case a: …; default: …; }`
    ///
    /// Case tests are evaluated in source order using strict equality; clause
    /// bodies fall through to the next clause unless terminated. `break` exits
    /// the switch, `continue` propagates to the enclosing loop.
    fn lower_switch(&mut self, switch: &SwitchStatement<'_>) {
        let disc = self.lower_expression(&switch.discriminant);
        let after_blk = self.create_block("switch_after");

        let n = switch.cases.len();
        let mut test_blks: Vec<Option<BlockId>> = Vec::with_capacity(n);
        let mut body_blks: Vec<BlockId> = Vec::with_capacity(n);
        let mut default_idx: Option<usize> = None;

        for (i, case) in switch.cases.iter().enumerate() {
            if case.test.is_some() {
                test_blks.push(Some(self.create_block(format!("case_test_{i}"))));
            } else {
                test_blks.push(None);
                default_idx = Some(i);
            }
            body_blks.push(self.create_block(format!("case_body_{i}")));
        }

        // `break` leaves the switch; `continue` belongs to the enclosing loop.
        let continue_point = self
            .loop_contexts
            .last()
            .map(|c| c.continue_point)
            .unwrap_or(after_blk);
        self.enter_loop_context(after_blk, continue_point);

        // Entry point of the test chain (or the default clause, or the exit).
        let tests: Vec<(usize, BlockId)> = test_blks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.map(|b| (i, b)))
            .collect();

        if let Some((_, first)) = tests.first() {
            self.builder.jump(*first);
        } else if let Some(d) = default_idx {
            self.builder.jump(body_blks[d]);
        } else {
            self.builder.jump(after_blk);
        }

        // Test chain: `disc === test ? body : next-test (or default, or exit)`.
        for (k, (i, test_blk)) in tests.iter().enumerate() {
            self.builder.switch_to_block(*test_blk);
            let case_test = switch.cases[*i].test.as_ref().unwrap();
            let test_val = self.lower_expression(case_test);
            let eq = self.builder.binop(Opcode::StrictEqual, disc, test_val);
            let next = tests
                .get(k + 1)
                .map(|(_, b)| *b)
                .or_else(|| default_idx.map(|d| body_blks[d]))
                .unwrap_or(after_blk);
            self.builder.br_if(eq, body_blks[*i], next);
        }

        // Clause bodies, falling through in source order.
        for i in 0..n {
            self.builder.switch_to_block(body_blks[i]);
            for stmt in &switch.cases[i].consequent {
                self.lower_statement(stmt);
                if self.current_block_is_terminated() {
                    break;
                }
            }
            if !self.current_block_is_terminated() {
                if i + 1 < n {
                    self.builder.jump(body_blks[i + 1]);
                } else {
                    self.builder.jump(after_blk);
                }
            }
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
        self.scope_depth += 1;
        for stmt in &block.body {
            self.lower_statement(stmt);
            if self.current_block_is_terminated() {
                break;
            }
        }
        self.scope_depth -= 1;
        self.symbols.leave_scope();
    }

    /// Emit a jump out of a loop, running any intervening `finally` blocks first.
    ///
    /// `DelayedJump` cannot carry block arguments, so it targets a small
    /// trampoline block which then performs an ordinary `Jump`. That way the
    /// SSA builder still inserts (and fills) the phi parameters the real target
    /// block needs — otherwise loop-carried variables would read stale
    /// registers after the finally block ran.
    fn lower_loop_exit(&mut self, target: BlockId, label: &str) {
        let Some(ctx_seh_depth) = self.loop_contexts.last().map(|c| c.seh_depth) else {
            return;
        };
        let seh_depth_to_pop = self.seh_depth.saturating_sub(ctx_seh_depth);
        if seh_depth_to_pop > 0 {
            // `DelayedJump` carries no block arguments, so it targets a small
            // trampoline which then performs an ordinary `Jump`. That way the
            // SSA builder inserts (and fills) the phi parameters the real target
            // needs; jumping straight there would leave loop-carried variables
            // holding stale registers.
            let trampoline = self.create_block(format!("{label}_trampoline"));
            // Every `finally` frame being unwound leads here, so register the
            // trampoline on each of them (see `SehFrameInfo::pending_exits`).
            let first = self.seh_frames.len().saturating_sub(seh_depth_to_pop);
            for frame in self.seh_frames.iter_mut().skip(first) {
                frame.pending_exits.push(trampoline);
            }
            self.builder.delayed_jump(trampoline, seh_depth_to_pop);
            self.builder.switch_to_block(trampoline);
            self.builder.jump(target);
        } else {
            self.builder.jump(target);
        }
        self.builder.seal_block(self.builder.current_block());
    }

    fn lower_break(&mut self) {
        if let Some(ctx) = self.loop_contexts.last() {
            let break_point = ctx.break_point;
            self.lower_loop_exit(break_point, "break");
        } else {
            log::warn!("break outside loop - ignoring");
        }
    }

    fn lower_continue(&mut self) {
        if let Some(ctx) = self.loop_contexts.last() {
            let continue_point = ctx.continue_point;
            self.lower_loop_exit(continue_point, "continue");
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
        self.seh_frames.push(SehFrameInfo {
            finally_blk: if has_finally { Some(finally_blk) } else { None },
            pending_exits: Vec::new(),
        });

        self.builder.push_seh(seh_handler, seh_finally);
        self.builder.add_exception_edge(try_body, seh_handler);
        if has_finally {
            self.builder.add_exception_edge(try_body, finally_blk);
            // A `throw` from inside the catch clause also runs this `finally`
            // before propagating outwards — model that as a real CFG edge, or
            // values written in the catch clause look dead to liveness/SSA.
            if has_catch {
                self.builder.add_exception_edge(catch_blk, finally_blk);
            }
        }
        self.builder.jump(try_body);

        // Try body
        self.builder.switch_to_block(try_body);
        if let Some(v) = self.lower_block_like_with_result(&try_stmt.block.body) {
            self.builder.assign(result_var, v);
        }
        // Decrement SEH depth after try body
        self.seh_depth -= 1;
        // Model the data flow a delayed break/continue creates: the trampoline
        // is entered once this try's `finally` has finished, so the finally
        // block is its real predecessor.
        if let Some(frame) = self.seh_frames.pop() {
            if let Some(finally_blk) = frame.finally_blk {
                for target in frame.pending_exits {
                    self.builder
                        .control_flow_graph_mut()
                        .add_edge(finally_blk, target);
                }
            }
        }
        if !self.current_block_is_terminated() {
            // Emit into whatever block the body *ended* in. Switching back to
            // `try_body` would append the normal-exit path after the body's
            // terminator (dead code) and leave the real tail block unterminated,
            // so it would silently fall through into the next block.
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
                self.scope_depth += 1;
                let name = self.catch_clause_param_name(catch_clause);
                let dst = self.builder.alloc();
                self.builder.assign(dst, exc_val);
                self.symbols.insert(name, Variable::new(dst));
                if let Some(v) = self.lower_block_like_with_result(&catch_clause.body.body) {
                    self.builder.assign(result_var, v);
                }
                self.scope_depth -= 1;
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
            // `yield` / `yield*` (ES 14.4).
            Expression::YieldExpression(y) if !y.delegate => {
                let src = y
                    .argument
                    .as_ref()
                    .map(|arg| self.lower_expression(arg));
                self.builder.yield_(src)
            }
            Expression::YieldExpression(y) => {
                // `yield*` cannot be a single instruction: the value sent by
                // the next `next(v)` has to reach the delegate's `next`, which
                // is a loop over the iterator result protocol.
                let arg = y
                    .argument
                    .as_ref()
                    .expect("`yield*` always has an argument");
                self.lower_yield_delegate(arg)
            }
            Expression::MetaProperty(meta) => {
                // `new.target` (ES 14.2.3) reads the current frame's constructor
                // slot: the constructor for a `[[Construct]]` frame, otherwise
                // `undefined`. Inside an arrow it is the value captured when the
                // arrow object was created.
                if meta.meta.name == "new" && meta.property.name == "target" {
                    self.builder.load_new_target()
                } else {
                    // `import.meta` requires a module loader (out of scope).
                    log::warn!(
                        "unsupported meta property: {}.{}",
                        meta.meta.name,
                        meta.property.name
                    );
                    Value::Primitive(Primitive::Undefined)
                }
            }
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
            // Script-scope globals live in the global environment and can be
            // written by *any* function (or via `window.x` style access), so a
            // cached register copy would go stale. Re-read them with LoadEnv.
            if self.global_names.contains(ident.name.as_str()) {
                return self.builder.load_external_variable(ident.name.to_string());
            }
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
            BinaryOperator::Exponential => self.builder.binop(Opcode::Pow, lhs, rhs),
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
            BinaryOperator::BitwiseAnd => self.builder.binop(Opcode::BitAnd, lhs, rhs),
            BinaryOperator::BitwiseOR => self.builder.binop(Opcode::BitOr, lhs, rhs),
            BinaryOperator::BitwiseXOR => self.builder.binop(Opcode::BitXor, lhs, rhs),
            BinaryOperator::ShiftLeft => self.builder.binop(Opcode::Shl, lhs, rhs),
            BinaryOperator::ShiftRight => self.builder.binop(Opcode::Shr, lhs, rhs),
            BinaryOperator::ShiftRightZeroFill => self.builder.binop(Opcode::UShr, lhs, rhs),
            _ => {
                log::warn!("unimplemented binary operator: {:?}", bin.operator);
                Value::Primitive(Primitive::Null)
            }
        }
    }

    fn lower_unary(&mut self, unary: &UnaryExpression<'_>) -> Value {
        // `delete` and `typeof` need the *reference*, not the loaded value, so
        // the operand is lowered lazily per operator.
        if let UnaryOperator::Delete = unary.operator {
            return self.lower_delete(&unary.argument);
        }
        if let UnaryOperator::Typeof = unary.operator {
            return self.lower_typeof(&unary.argument);
        }

        let arg = self.lower_expression(&unary.argument);

        match unary.operator {
            UnaryOperator::UnaryNegation => self.builder.unaryop(Opcode::Neg, arg),
            // `+expr` → ToNumber(expr). It must not go through `Addx`, whose
            // string operand triggers concatenation (`+"5"` was `"05"`).
            UnaryOperator::UnaryPlus => self.builder.unaryop(Opcode::ToNumber, arg),
            UnaryOperator::LogicalNot => self.builder.unaryop(Opcode::Not, arg),
            UnaryOperator::BitwiseNot => {
                // ~expr → bitwise NOT
                self.builder.unaryop(Opcode::BitNot, arg)
            }
            UnaryOperator::Typeof => self.builder.typeof_(arg),
            UnaryOperator::Void => {
                // `void expr`: evaluate for side effects, always yield undefined.
                // `arg` above already emitted the evaluation.
                Value::Primitive(Primitive::Undefined)
            }
            UnaryOperator::Delete => unreachable!("handled above"),
        }
    }

    /// `typeof expr`.
    ///
    /// An unresolvable identifier yields `"undefined"` instead of raising a
    /// `ReferenceError` (ES6 12.5.5.1 — IsUnresolvableReference short-circuit).
    fn lower_typeof(&mut self, expr: &Expression<'_>) -> Value {
        if let Expression::Identifier(ident) = expr.get_inner_expression() {
            let name = ident.name.as_str();
            // Names the compiler cannot resolve might still be globals that are
            // only installed at run time (`Object`, `SyntaxError`, …), so the
            // "undefined" fallback has to happen in the VM.
            if !matches!(name, "undefined" | "NaN" | "Infinity")
                && self.symbols.lookup(name).is_none()
                && !self.global_names.contains(name)
            {
                return self.builder.typeof_env(name.to_string());
            }
        }
        let arg = self.lower_expression(expr);
        self.builder.typeof_(arg)
    }

    /// `delete target` — removes a property and reports whether it existed.
    fn lower_delete(&mut self, expr: &Expression<'_>) -> Value {
        match expr.get_inner_expression() {
            Expression::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                self.builder
                    .delete_property(object, member.property.name.as_str())
            }
            Expression::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                self.builder.delete_index(object, index)
            }
            // `delete someIdentifier` — bindings are not deletable here.
            _ => {
                if let Expression::Identifier(_) = expr.get_inner_expression() {
                    Value::Primitive(Primitive::Boolean(false))
                } else {
                    log::warn!("unsupported delete target");
                    Value::Primitive(Primitive::Boolean(true))
                }
            }
        }
    }

    /// `ToNumber(current) \u{00b1} 1`, returning `(oldNumeric, newValue)`.
    ///
    /// ES 13.4.4/13.4.5 operate on `ToNumeric(GetValue(x))`, and the postfix
    /// form yields that *converted* value: `var s = "5"; s++` yields the number
    /// 5 (and stores 6), not the string `"5"`.
    fn lower_update_step(&mut self, current: Value, op: Opcode) -> (Value, Value) {
        let one = Value::Primitive(Primitive::Float(1.0));
        let numeric = self.builder.unaryop(Opcode::ToNumber, current);
        let new_val = self.builder.binop(op, numeric, one);
        (numeric, new_val)
    }

    fn lower_update(&mut self, update: &UpdateExpression<'_>) -> Value {
        let op = match update.operator {
            UpdateOperator::Increment => Opcode::Addx,
            UpdateOperator::Decrement => Opcode::Subx,
        };

        match &update.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
                // Script-scope globals must round-trip through the global
                // environment: the cached register copy may be stale after a
                // nested function stored to the same global.
                if self.global_names.contains(ident.name.as_str()) {
                    let current = self
                        .builder
                        .load_external_variable(ident.name.to_string());
                    let (old_num, new_val) = self.lower_update_step(current, op);
                    self.builder
                        .store_external_variable(ident.name.to_string(), new_val);
                    return if update.prefix { new_val } else { old_num };
                }
                match self.symbols.lookup(ident.name.as_str()) {
                    Some(var) => {
                        let current = var.0;
                        let (old_num, new_val) = self.lower_update_step(current, op);
                        self.builder.assign(current, new_val);
                        self.sync_global(&ident.name.to_string(), new_val);
                        // prefix returns new value, postfix returns old value
                        if update.prefix { new_val } else { old_num }
                    }
                    None => {
                        // Undeclared target: treat as a global slot.
                        let current = self
                            .builder
                            .load_external_variable(ident.name.to_string());
                        let (old_num, new_val) = self.lower_update_step(current, op);
                        self.builder
                            .store_external_variable(ident.name.to_string(), new_val);
                        if update.prefix { new_val } else { old_num }
                    }
                }
            }
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let current = self
                    .builder
                    .get_property(object, member.property.name.as_str());
                let (old_num, new_val) = self.lower_update_step(current, op);
                self.builder
                    .set_property(object, member.property.name.as_str(), new_val);
                if update.prefix { new_val } else { old_num }
            }
            SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                let current = self.builder.index_get(object, index);
                let (old_num, new_val) = self.lower_update_step(current, op);
                self.builder.index_set(object, index, new_val);
                if update.prefix { new_val } else { old_num }
            }
            _ => {
                log::warn!(
                    "unsupported update expression target: {:?}",
                    update.argument
                );
                Value::Primitive(Primitive::Null)
            }
        }
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
                // `??` short-circuits on null/undefined: the rhs is evaluated
                // only when the lhs is nullish, and the lhs is returned as-is
                // otherwise (so `0 ?? x` is `0`, unlike `0 || x`).
                let result = self.builder.alloc();
                let rhs_blk = self.create_block("coalesce_rhs");
                let merge_blk = self.create_block("coalesce_merge");

                // Loose equality with null is true for exactly null and
                // undefined, which is the trigger condition for `??`.
                let is_nullish = self.builder.binop(
                    Opcode::Equal,
                    lhs.clone(),
                    Value::Primitive(Primitive::Null),
                );

                self.builder.assign(result, lhs);
                self.builder.br_if(is_nullish, rhs_blk, merge_blk);

                self.builder.switch_to_block(rhs_blk);
                let rhs = self.lower_expression(&logical.right);
                self.builder.assign(result, rhs);
                self.builder.jump(merge_blk);

                self.builder.switch_to_block(merge_blk);
                result
            }
        }
    }

    /// Emit `current <op> rhs` for a compound assignment (`+=`, `&=`, …).
    fn compound_binop(
        &mut self,
        op: AssignmentOperator,
        current: Value,
        rhs: Value,
    ) -> Option<Value> {
        let opcode = match op {
            AssignmentOperator::Addition => Opcode::Addx,
            AssignmentOperator::Subtraction => Opcode::Subx,
            AssignmentOperator::Multiplication => Opcode::Mulx,
            AssignmentOperator::Division => Opcode::Divx,
            AssignmentOperator::Remainder => Opcode::Remx,
            AssignmentOperator::Exponential => Opcode::Pow,
            AssignmentOperator::BitwiseAnd => Opcode::BitAnd,
            AssignmentOperator::BitwiseOR => Opcode::BitOr,
            AssignmentOperator::BitwiseXOR => Opcode::BitXor,
            AssignmentOperator::ShiftLeft => Opcode::Shl,
            AssignmentOperator::ShiftRight => Opcode::Shr,
            AssignmentOperator::ShiftRightZeroFill => Opcode::UShr,
            _ => return None,
        };
        Some(self.builder.binop(opcode, current, rhs))
    }

    fn lower_assignment(&mut self, assign: &AssignmentExpression<'_>) -> Value {
        match &assign.left {
            AssignmentTarget::AssignmentTargetIdentifier(ident) => {
                // Script-scope globals: the global environment is authoritative.
                // Writes go through StoreEnv and reads (for compound ops) via a
                // fresh LoadEnv, so nested functions observe the same value.
                if self.global_names.contains(ident.name.as_str()) {
                    let rhs = self.lower_expression(&assign.right);
                    let value = if assign.operator == AssignmentOperator::Assign {
                        rhs
                    } else {
                        let current = self
                            .builder
                            .load_external_variable(ident.name.to_string());
                        self.compound_binop(assign.operator, current, rhs)
                            .unwrap_or(rhs)
                    };
                    self.builder
                        .store_external_variable(ident.name.to_string(), value);
                    return value;
                }
                match self.symbols.lookup(ident.name.as_str()) {
                    Some(var) => {
                        let current = var.0;
                        let rhs = self.lower_expression(&assign.right);
                        let value = self
                            .compound_binop(assign.operator, current, rhs)
                            .unwrap_or(rhs);
                        self.builder.assign(current, value);
                        self.sync_global(&ident.name.to_string(), value);
                        value
                    }
                    None => {
                        // Assignment to an undeclared (or environment-backed)
                        // name: read-modify-write through the global environment.
                        let rhs = self.lower_expression(&assign.right);
                        let value = if assign.operator == AssignmentOperator::Assign {
                            rhs
                        } else {
                            let current = self
                                .builder
                                .load_external_variable(ident.name.to_string());
                            self.compound_binop(assign.operator, current, rhs)
                                .unwrap_or(rhs)
                        };
                        self.builder
                            .store_external_variable(ident.name.to_string(), value);
                        value
                    }
                }
            }
            AssignmentTarget::ComputedMemberExpression(member) => {
                // Evaluate the reference base first, then the RHS (spec order).
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                let rhs = self.lower_expression(&assign.right);
                let value = if assign.operator == AssignmentOperator::Assign {
                    rhs
                } else {
                    let current = self.builder.index_get(object, index);
                    self.compound_binop(assign.operator, current, rhs)
                        .unwrap_or(rhs)
                };
                self.builder.index_set(object, index, value);
                value
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let rhs = self.lower_expression(&assign.right);
                let value = if assign.operator == AssignmentOperator::Assign {
                    rhs
                } else {
                    let current = self
                        .builder
                        .get_property(object, member.property.name.as_str());
                    self.compound_binop(assign.operator, current, rhs)
                        .unwrap_or(rhs)
                };
                self.builder
                    .set_property(object, member.property.name.as_str(), value);
                value
            }
            // Destructuring assignment (`[a, b] = arr`, `({x} = obj)`). A
            // compound operator makes no sense here, so only `=` is honoured;
            // the RHS is evaluated first, per spec order.
            AssignmentTarget::ArrayAssignmentTarget(target) => {
                let rhs = self.lower_expression(&assign.right);
                self.bind_array_assignment_target(target, rhs);
                rhs
            }
            AssignmentTarget::ObjectAssignmentTarget(target) => {
                let rhs = self.lower_expression(&assign.right);
                self.bind_object_assignment_target(target, rhs);
                rhs
            }
            _ => {
                log::warn!("unimplemented assignment target: {:?}", assign.left);
                self.lower_expression(&assign.right)
            }
        }
    }

    // --------------------------- spread ---------------------------

    /// Does this argument list contain a `...` element?
    fn has_spread_arguments(args: &[Argument<'_>]) -> bool {
        args.iter()
            .any(|arg| matches!(arg, Argument::SpreadElement(_)))
    }

    /// Build the array of call arguments for `f(...)` / `new F(...)`,
    /// appending the elements of every `...src` argument.
    fn lower_spread_arguments(&mut self, args: &[Argument<'_>]) -> Value {
        let argv = self.builder.make_array();
        for arg in args {
            match arg {
                Argument::SpreadElement(spread) => {
                    let src = self.lower_expression(&spread.argument);
                    self.emit_iterate_push(argv, src);
                }
                _ => {
                    let value = self.lower_argument_expr(arg);
                    self.builder.array_push(argv, value);
                }
            }
        }
        argv
    }

    /// `array.push(...src)` — one VM instruction instead of a lowered loop.
    fn emit_iterate_push(&mut self, array: Value, src: Value) {
        self.builder.array_push_spread(array, src);
    }

    fn lower_call(&mut self, call: &CallExpression<'_>) -> Value {
        // `super(...)` — call the parent constructor with the current `this`.
        if matches!(&call.callee, Expression::Super(_)) {
            // No `load_this` here on purpose: in a derived constructor `this` is
            // uninitialized until `super()` runs, and reading it would raise.
            // `CallSuperSpread` takes the receiver from the frame instead.
            let this = Value::Primitive(Primitive::Undefined);
            let super_ctor = self.lower_super_ctor();
            // `this` is passed to `call_spread` separately, so it must not
            // appear in the argument array as well.
            let argv = self.builder.make_array();
            for arg in &call.arguments {
                match arg {
                    Argument::SpreadElement(spread) => {
                        let src = self.lower_expression(&spread.argument);
                        self.emit_iterate_push(argv, src);
                    }
                    _ => {
                        let value = self.lower_argument_expr(arg);
                        self.builder.array_push(argv, value);
                    }
                }
            }
            return self.builder.call_super(super_ctor, this, argv);
        }

        let has_spread = Self::has_spread_arguments(&call.arguments);
        let args: Vec<Value> = call
            .arguments
            .iter()
            .map(|arg| match arg {
                // A spread element is folded into the argv array above; the
                // per-argument slot stays a placeholder in that case.
                Argument::SpreadElement(_) => Value::Primitive(Primitive::Null),
                _ => self.lower_argument_expr(arg),
            })
            .collect();

        // `...` in the argument list: build an argv array and let the VM do the
        // call with an explicit `this`.
        if has_spread {
            let argv = self.lower_spread_arguments(&call.arguments);
            let (callee, this) = match &call.callee {
                Expression::StaticMemberExpression(static_member) => {
                    let object = self.lower_expression(&static_member.object);
                    let prop_name = static_member.property.name.as_str();
                    let method = self.builder.get_property(object, prop_name);
                    (method, object)
                }
                Expression::ComputedMemberExpression(computed_member) => {
                    let object = self.lower_expression(&computed_member.object);
                    let prop = self.lower_expression(&computed_member.expression);
                    let method = self.builder.index_get(object, prop);
                    (method, object)
                }
                _ => {
                    let callee = self.lower_expression(&call.callee);
                    (callee, Value::Primitive(Primitive::Undefined))
                }
            };
            return self.builder.call_spread(callee, this, argv);
        }

        // Check if callee is a member expression (method call)
        match &call.callee {
            Expression::StaticMemberExpression(static_member) => {
                // `super.m(...)`: the parent method must run with the current
                // `this`, which plain property access cannot provide, so route
                // it through `Function.prototype.call`.
                if matches!(&static_member.object, Expression::Super(_)) {
                    let this = self.builder.load_this();
                    let super_proto = self.lower_super_proto();
                    let method = self
                        .builder
                        .get_property(super_proto, static_member.property.name.as_str());
                    let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                    call_args.push(this);
                    call_args.extend(args.iter().copied());
                    return self.builder.call_property(method, "call", call_args);
                }
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
        if Self::has_spread_arguments(&new.arguments) {
            let argv = self.lower_spread_arguments(&new.arguments);
            return self.builder.new_spread(constructor, argv);
        }
        let args: Vec<Value> = new
            .arguments
            .iter()
            .map(|arg| self.lower_argument_expr(arg))
            .collect();

        self.builder.new_(constructor, args)
    }

    fn lower_member_expr(&mut self, expr: &Expression<'_>) -> Value {
        match expr {
            Expression::Super(_) => self.lower_super_proto(),
            Expression::StaticMemberExpression(static_member) => {
                // `super.name` — read it from the parent's prototype.
                if matches!(&static_member.object, Expression::Super(_)) {
                    let super_proto = self.lower_super_proto();
                    return self
                        .builder
                        .get_property(super_proto, static_member.property.name.as_str());
                }
                let object = self.lower_expression(&static_member.object);
                self.builder
                    .get_property(object, static_member.property.name.as_str())
            }
            Expression::ComputedMemberExpression(computed) => {
                if matches!(&computed.object, Expression::Super(_)) {
                    let super_proto = self.lower_super_proto();
                    let index = self.lower_expression(&computed.expression);
                    return self.builder.index_get(super_proto, index);
                }
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

    // --------------------------- `super` ---------------------------

    /// The prototype `super` reads from, as recorded on the running function
    /// when its class (or object literal) was defined.
    ///
    /// Deriving it from `this` instead would break in the middle of an
    /// inheritance chain: `this` belongs to the most-derived class, so an
    /// intermediate `super.m()` would resolve back to itself.
    fn lower_super_proto(&mut self) -> Value {
        let current = self.builder.load_current_function();
        self.builder.get_property(current, "__superProto__")
    }

    /// The parent constructor of the *running* constructor.
    ///
    /// The parent is recorded as `__super__` on each class constructor when the
    /// class is defined. Reading it back from the running function (rather than
    /// deriving it from `this`) is what makes `super()` work in the middle of an
    /// inheritance chain: `this` always belongs to the most-derived class, so a
    /// receiver-based lookup would resolve every intermediate `super()` to the
    /// same parent and recurse forever.
    fn lower_super_ctor(&mut self) -> Value {
        let current = self.builder.load_current_function();
        self.builder.get_property(current, "__super__")
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
                ArrayExpressionElement::SpreadElement(spread) => {
                    let src = self.lower_expression(&spread.argument);
                    self.emit_iterate_push(array, src);
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

    /// Lower a property key written in source (`{[k]: v}`, `class { [k]() {} }`).
    ///
    /// A non-computed literal stays a static string, so the common case keeps
    /// the cheap constant path; everything else (including a computed key whose
    /// expression happens to be a literal) is evaluated here, in source order.
    fn lower_member_key(&mut self, key: &PropertyKey<'_>, computed: bool) -> MemberKey {
        if computed {
            return MemberKey::Dynamic(self.lower_property_key_expression(key));
        }
        match key {
            PropertyKey::StaticIdentifier(id) => MemberKey::Static(id.name.to_string()),
            PropertyKey::StringLiteral(lit) => MemberKey::Static(lit.value.to_string()),
            PropertyKey::NumericLiteral(lit) => {
                MemberKey::Static(crate::builtins::number_to_string(lit.value))
            }
            _ => MemberKey::Dynamic(self.lower_property_key_expression(key)),
        }
    }

    /// `target[key] = value`, or `target.name = value` for a static key.
    fn set_member(&mut self, target: Value, key: &MemberKey, value: Value) {
        match key {
            MemberKey::Static(name) => self.builder.set_property(target, name, value),
            MemberKey::Dynamic(key_value) => {
                self.builder.set_property_dynamic(target, key_value.clone(), value)
            }
        }
    }

    /// `Object.defineProperty(target, key, descriptor)` with a key that may only
    /// be known at run time.
    fn define_member(&mut self, target: Value, key: &MemberKey, desc: Value) {
        let key_value = match key {
            MemberKey::Static(name) => self.builder.load_constant(
                crate::bytecode::Constant::String(std::sync::Arc::new(name.clone().into())),
            ),
            MemberKey::Dynamic(value) => value.clone(),
        };
        let object_fn = self.builder.load_external_variable("Object".to_string());
        self.builder.call_property(
            object_fn,
            "defineProperty",
            vec![target, key_value, desc],
        );
    }

    /// `Object.defineProperty` descriptor for a method-like member.
    ///
    /// Matches `CreateMethodProperty`: writable and configurable, and
    /// **non-enumerable** — that is what keeps `Object.keys(C.prototype)` empty.
    fn method_descriptor(&mut self, value: Value) -> Value {
        let desc = self.builder.make_object();
        self.builder.set_property(desc, "value", value);
        self.builder
            .set_property(desc, "writable", Value::Primitive(Primitive::Boolean(true)));
        self.builder.set_property(
            desc,
            "enumerable",
            Value::Primitive(Primitive::Boolean(false)),
        );
        self.builder.set_property(
            desc,
            "configurable",
            Value::Primitive(Primitive::Boolean(true)),
        );
        desc
    }

    /// `Object.defineProperty` descriptor for an accessor member.
    ///
    /// Only the halves that exist are mentioned, so `[[DefineOwnProperty]]`
    /// merges a `get`/`set` pair that was declared as two separate members.
    fn accessor_descriptor(
        &mut self,
        getter: Option<Value>,
        setter: Option<Value>,
        enumerable: bool,
    ) -> Value {
        let desc = self.builder.make_object();
        if let Some(getter) = getter {
            self.builder.set_property(desc, "get", getter);
        }
        if let Some(setter) = setter {
            self.builder.set_property(desc, "set", setter);
        }
        self.builder.set_property(
            desc,
            "enumerable",
            Value::Primitive(Primitive::Boolean(enumerable)),
        );
        self.builder.set_property(
            desc,
            "configurable",
            Value::Primitive(Primitive::Boolean(true)),
        );
        desc
    }

    fn lower_object(&mut self, obj: &ObjectExpression<'_>) -> Value {
        let object = self.builder.make_object();
        for prop in &obj.properties {
            match prop {
                ObjectPropertyKind::ObjectProperty(p) => {
                    // Per ES 12.2.6.8 the key is evaluated before the value.
                    let key = self.lower_member_key(&p.key, p.computed);
                    // A method's [[HomeObject]] is the literal itself, so `super.x`
                    // must read from `getPrototypeOf(object)`.
                    let home = p.method.then(|| self.get_prototype_of(object));
                    match p.kind {
                        PropertyKind::Get => {
                            let getter = self.lower_expression(&p.value);
                            let getter = match &home {
                                Some(super_proto) => {
                                    self.attach_super_proto(getter, super_proto.clone())
                                }
                                None => getter,
                            };
                            let desc = self.accessor_descriptor(Some(getter), None, true);
                            self.define_member(object, &key, desc);
                        }
                        PropertyKind::Set => {
                            let setter = self.lower_expression(&p.value);
                            let setter = match &home {
                                Some(super_proto) => {
                                    self.attach_super_proto(setter, super_proto.clone())
                                }
                                None => setter,
                            };
                            let desc = self.accessor_descriptor(None, Some(setter), true);
                            self.define_member(object, &key, desc);
                        }
                        PropertyKind::Init => {
                            let value = self.lower_expression(&p.value);
                            let value = match &home {
                                Some(super_proto) => {
                                    self.attach_super_proto(value, super_proto.clone())
                                }
                                None => value,
                            };
                            self.set_member(object, &key, value);
                        }
                    }
                }
                ObjectPropertyKind::SpreadProperty(spread) => {
                    let src = self.lower_expression(&spread.argument);
                    self.emit_object_spread(object, src);
                }
            }
        }
        object
    }

    fn lower_arrow_function(&mut self, arrow: &ArrowFunctionExpression<'_>) -> Value {
        // An anonymous arrow's `name` is the empty string (ES 14.2.16); name
        // inference from the assignment target is not implemented.
        let name = String::new();

        // Arrow functions with expression body need automatic return
        let is_expression_body = arrow.expression;

        // Determine which outer variables need to be captured.
        // We collect all referenced identifiers and subtract those declared inside the arrow body.
        let param_names: std::collections::HashSet<String> = arrow
            .params
            .items
            .iter()
            .map(|p| self.binding_pattern_name(&p.pattern))
            .collect();
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
            &arrow.params.items,
            &arrow.body,
            None,
            is_expression_body,
            &free_idents
                .iter()
                .map(|s| String::from(*s))
                .collect::<Vec<String>>(),
            true,
            &[],
            false,
            false,);

        // At runtime, capture the current `this` value. No `load_this` here on
        // purpose: in a derived constructor `this` is still uninitialized, and
        // an arrow that only calls `super()` must not trip over reading it —
        // `MakeArrowFuncObj` takes the frame's `this` instead.

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
        self.builder
            .make_arrow_func_obj(func_val, Value::Primitive(Primitive::Undefined))
    }

    fn lower_function_expr(&mut self, func: &Function<'_>) -> Value {
        let name = func
            .id
            .as_ref()
            .map(|id| id.name.to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        if let Some(body) = &func.body {
            self.lower_function_inner(
                Some(name),
                &func.params.items,
                body,
                None,
                false,
                &[],
                false,
                &[],
                func.generator,
                false,)
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
        // Interleave cooked quasis with ToString'd expressions:
        //   `a${x}b` → "a" ++ ToString(x) ++ "b"
        // ToString (not Addx) is required: with plain Addx, two leading
        // numbers would add arithmetically instead of concatenating.
        let mut result: Option<Value> = None;

        for (i, quasi) in tpl.quasis.iter().enumerate() {
            let cooked = quasi.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
            if !cooked.is_empty() {
                let part = self.builder.load_constant(cooked.into());
                result = Some(match result {
                    None => part,
                    Some(prev) => self.builder.binop(Opcode::Addx, prev, part),
                });
            }

            if let Some(expr) = tpl.expressions.get(i) {
                let value = self.lower_expression(expr);
                let part = self.builder.to_string(value);
                result = Some(match result {
                    None => part,
                    Some(prev) => self.builder.binop(Opcode::Addx, prev, part),
                });
            }
        }

        result.unwrap_or(Value::Primitive(Primitive::Undefined))
    }

    // ──────────────────────── Function Lowering ────────────────────────

    /// Collect a function declaration for hoisting (returns name and function value).
    fn collect_function_declaration(&mut self, func: &Function<'_>) -> Option<(String, Value)> {
        let name = func
            .id
            .as_ref()
            .map(|id| id.name.to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        if let Some(body) = &func.body {
            let func_id_val = self.lower_function_inner(
                Some(name.clone()),
                &func.params.items,
                body,
                None,
                false,
                &[],
                false,
                &[],
                func.generator,
                false,);
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
        params: &[FormalParameter<'_>],
        body: &FunctionBody<'_>,
        captured_this: Option<Value>,
        auto_return: bool,
        captured_names: &[String],
        is_arrow: bool,
        instance_fields: &[&PropertyDefinition<'_>],
        is_generator: bool,
        is_derived_ctor: bool,
    ) -> Value {
        // During hoisting, there may be no current block yet
        let curr = self.builder.try_current_block();

        let sig_params: Vec<FuncParam> = params
            .iter()
            .map(|p| FuncParam::new(self.binding_pattern_name(&p.pattern)))
            .collect();
        // `fn.length` counts only the leading parameters without an initializer
        // (`function f(a, b = 1, c)` has length 1). Rest parameters live in
        // `FormalParameters::rest`, so they are not part of `params` at all.
        let arity = params
            .iter()
            .take_while(|p| {
                p.initializer.is_none()
                    && !matches!(p.pattern, BindingPattern::AssignmentPattern(_))
            })
            .count();
        let func_sig = FuncSignature::with_arity(name.clone(), sig_params, arity);
        let func_sig = if is_generator {
            func_sig.as_generator()
        } else {
            func_sig
        };
        let func_sig = if is_derived_ctor {
            func_sig.as_derived_ctor()
        } else {
            func_sig
        };
        let func_id = self.builder.module_mut().declare_function(func_sig.clone());

        let mut func = IrFunction::new(func_id, func_sig);

        // Clone outer symbols but exclude captured names so they fall through to LoadEnv
        let mut symbols = self.symbols.clone();
        for name in captured_names {
            symbols.remove(name);
        }
        // Script-scope bindings live in the global environment. They must NOT be
        // carried into the nested function's symbol table: the corresponding IR
        // variable belongs to the *outer* frame's register file, so using it here
        // would read a foreign (or, after SSA renaming, an arbitrary) value.
        if !self.global_names.is_empty() {
            for name in &self.global_names {
                symbols.remove(name);
            }
        }

        let mut func_builder = FunctionBuilder::new(self.builder.module_mut(), &mut func);
        let mut func_lower = JSASTLower::new(&mut func_builder, symbols);

        // Set up arrow function this capture if provided
        func_lower.arrow_this_var = captured_this;

        let entry = func_lower.create_block(name.clone().unwrap_or_else(|| "<fn>".to_string()));
        func_lower.builder.set_entry(entry);
        func_lower.builder.switch_to_block(entry);

        // Load arguments and bind parameters. A parameter with an initializer
        // (default value) gets an `undefined` check: the initializer runs only
        // when the argument was not passed (or was passed as `undefined`).
        // Initializers may reference earlier parameters, so bindings happen in
        // declaration order before each initializer is lowered.
        for (idx, param) in params.iter().enumerate() {
            let arg = func_lower.builder.load_arg(idx);
            let param_name = func_lower.binding_pattern_name(&param.pattern);

            if let BindingPattern::BindingIdentifier(_) = &param.pattern {
                func_lower
                    .symbols
                    .insert(param_name, Variable::new(arg));
            }

            if let Some(init) = &param.initializer {
                let undefined = Value::Primitive(Primitive::Undefined);
                let is_undef = func_lower
                    .builder
                    .binop(Opcode::StrictEqual, arg, undefined);
                let default_blk = func_lower.create_block("param_default");
                let merge_blk = func_lower.create_block("param_merge");
                func_lower.builder.br_if(is_undef, default_blk, merge_blk);

                func_lower.builder.switch_to_block(default_blk);
                let default_val = func_lower.lower_expression(init);
                func_lower.builder.assign(arg, default_val);
                func_lower.builder.jump(merge_blk);

                func_lower.builder.switch_to_block(merge_blk);
            }

            // Destructuring parameter (`function f([a, b]) {}`): bind after
            // the default-value prologue so defaults feed into the pattern.
            if !matches!(&param.pattern, BindingPattern::BindingIdentifier(_)) {
                func_lower.bind_pattern(&param.pattern, arg, false);
            }
        }

        // Every ordinary function (`...args`): binds a fresh array of every argument `arguments` binding. It has to be
        // installed eagerly (not on first use) so that nested arrow functions,
        // which clone this symbol table, resolve `arguments` lexically.
        if !is_arrow {
            let args_val = func_lower.builder.arguments_object();
            func_lower
                .symbols
                .insert("arguments".to_string(), Variable::new(args_val));
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

        // Instance fields (`x = 1` in a class body) are initialized at the start
        // of the constructor. For a derived class the spec places them right
        // after `super()`; emitting them first is close enough for the common
        // case and keeps the lowering local.
        for field in instance_fields {
            let this = func_lower.builder.load_this();
            let value = match &field.value {
                Some(init) => func_lower.lower_expression(init),
                None => Value::Primitive(Primitive::Undefined),
            };
            match &field.key {
                oxc_ast::ast::PropertyKey::StaticIdentifier(id) => {
                    func_lower
                        .builder
                        .set_property(this, id.name.as_str(), value);
                }
                oxc_ast::ast::PropertyKey::StringLiteral(lit) => {
                    func_lower
                        .builder
                        .set_property(this, lit.value.as_str(), value);
                }
                _ => {
                    // Computed or numeric key: fall back to its string form.
                    let key_name = func_lower.property_key_to_string(&field.key);
                    func_lower
                        .builder
                        .set_property(this, key_name.as_str(), value);
                }
            }
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

    // ----------------------- Destructuring -----------------------

    /// Bind a destructuring `pattern` to an already-evaluated `value`.
    ///
    /// Array patterns iterate their source (iterator protocol); object
    /// patterns read properties; `AssignmentPattern` nodes supply defaults
    /// via an `undefined` check. Leaf identifiers become locals, and -- when
    /// `publish_global` is set -- are also published to the global
    /// environment (script-scope declarations).
    fn bind_pattern(&mut self, pattern: &BindingPattern<'_>, value: Value, publish_global: bool) {
        match pattern {
            BindingPattern::BindingIdentifier(id) => {
                self.symbols.insert(id.name.to_string(), Variable::new(value));
                if publish_global && self.at_script_scope() {
                    self.builder
                        .store_external_variable(id.name.to_string(), value.clone());
                    self.global_names.insert(id.name.to_string());
                }
            }
            BindingPattern::AssignmentPattern(ap) => {
                let result = self.builder.alloc();
                self.builder.assign(result, value.clone());
                let undefined = Value::Primitive(Primitive::Undefined);
                let is_undef = self
                    .builder
                    .binop(Opcode::StrictEqual, result.clone(), undefined);
                let default_val = self.eval_default_on(&[is_undef], &ap.right, result.clone());
                self.bind_pattern(&ap.left, default_val, publish_global);
            }
            BindingPattern::ObjectPattern(op) => {
                let mut consumed: Vec<Value> = Vec::new();
                for prop in &op.properties {
                    let (key_value, key_name, computed) =
                        self.lower_binding_property_key(&prop.key, prop.computed);
                    consumed.push(key_value.clone());

                    let prop_val = if computed {
                        self.builder.index_get(value.clone(), key_value)
                    } else {
                        self.builder
                            .get_property(value.clone(), key_name.unwrap_or_default().as_str())
                    };
                    self.bind_pattern(&prop.value, prop_val, publish_global);
                }

                if let Some(rest) = &op.rest {
                    let rest_obj = self.builder.make_object();
                    self.emit_rest_object(rest_obj, value.clone(), consumed);
                    self.bind_pattern(&rest.argument, rest_obj, publish_global);
                }
            }
            BindingPattern::ArrayPattern(ap) => {
                let it = self.builder.make_iterator(value);

                for element in &ap.elements {
                    match element {
                        // Elision hole: step the iterator, discard the value.
                        None => {
                            self.builder.iterate_next(it);
                        }
                        Some(pat) => {
                            let (item, has_next) = self.builder.iterate_next(it);
                            self.bind_array_element_pattern(pat, item, has_next, publish_global);
                        }
                    }
                }

                if let Some(rest) = &ap.rest {
                    let rest_arr = self.builder.make_array();
                    self.emit_rest_collect(rest_arr, it);
                    self.bind_pattern(&rest.argument, rest_arr, publish_global);
                }
                // The iterator completed normally; no IteratorClose needed.
            }
            _ => {
                log::warn!("unsupported binding pattern in destructuring");
            }
        }
    }

    /// Bind one element of an array pattern, honouring defaults.
    fn bind_array_element_pattern(
        &mut self,
        pat: &BindingPattern<'_>,
        item: Value,
        has_next: Value,
        publish_global: bool,
    ) {
        match pat {
            BindingPattern::AssignmentPattern(ap) => {
                // `x = default`: take the default when the element is missing
                // (iterator exhausted) or explicitly undefined. The two
                // conditions combine with a bitwise OR on booleans.
                let result = self.builder.alloc();
                self.builder.assign(result, item);
                let undefined = Value::Primitive(Primitive::Undefined);
                let is_undef = self
                    .builder
                    .binop(Opcode::StrictEqual, result.clone(), undefined);
                let no_next = self.builder.unaryop(Opcode::Not, has_next);
                let missing = self.builder.binop(Opcode::BitOr, no_next, is_undef);
                let default_val = self.eval_default_on(&[missing], &ap.right, result.clone());
                self.bind_pattern(&ap.left, default_val, publish_global);
            }
            other => self.bind_pattern(other, item, publish_global),
        }
    }

    /// Evaluate `default_expr` when any of `conds` is truthy; return the merged
    /// value (same IR variable on both paths, filled by SSA phi).
    fn eval_default_on(
        &mut self,
        conds: &[Value],
        default_expr: &Expression<'_>,
        result: Value,
    ) -> Value {
        let default_blk = self.create_block("destr_default");
        let merge_blk = self.create_block("destr_merge");

        for (i, cond) in conds.iter().enumerate() {
            let is_last = i + 1 == conds.len();
            let false_target = if is_last {
                merge_blk
            } else {
                self.create_block("destr_cond")
            };
            self.builder.br_if(*cond, default_blk, false_target);
            if !is_last {
                self.builder.switch_to_block(false_target);
            }
        }

        self.builder.switch_to_block(default_blk);
        let default_val = self.lower_expression(default_expr);
        self.builder.assign(result.clone(), default_val);
        self.builder.jump(merge_blk);

        self.builder.switch_to_block(merge_blk);
        result
    }

    /// Evaluate a `BindingProperty` key: returns (runtime key value, static
    /// property name, use index access).
    fn lower_binding_property_key(
        &mut self,
        key: &PropertyKey<'_>,
        computed: bool,
    ) -> (Value, Option<String>, bool) {
        match key {
            PropertyKey::StaticIdentifier(id) => {
                let kv = self.builder.load_constant(crate::bytecode::Constant::String(
                    std::sync::Arc::new(id.name.to_string().into()),
                ));
                (kv, Some(id.name.to_string()), false)
            }
            PropertyKey::StringLiteral(lit) => {
                let name = lit.value.to_string();
                let kv = self.builder.load_constant(crate::bytecode::Constant::String(
                    std::sync::Arc::new(name.clone().into()),
                ));
                (kv, Some(name), false)
            }
            PropertyKey::NumericLiteral(lit) => {
                let name = crate::builtins::number_to_string(lit.value);
                let kv = self.builder.load_constant(crate::bytecode::Constant::String(
                    std::sync::Arc::new(name.clone().into()),
                ));
                (kv, Some(name), false)
            }
            _ => {
                let kv = self.lower_property_key_expression(key);
                (kv, None, true || computed)
            }
        }
    }

    /// Lower a computed property key expression.
    fn lower_property_key_expression(&mut self, key: &PropertyKey<'_>) -> Value {
        match key {
            PropertyKey::Identifier(id) => self.lower_identifier(id),
            PropertyKey::StringLiteral(lit) => self.builder.load_constant(
                crate::bytecode::Constant::String(std::sync::Arc::new(
                    lit.value.to_string().into(),
                )),
            ),
            PropertyKey::NumericLiteral(lit) => self.lower_numeric_literal(lit),
            PropertyKey::TemplateLiteral(tpl) => self.lower_template_literal(tpl),
            // A computed key is an arbitrary expression (`[f()]`, `[a + b]`,
            // `[obj]`, …): lower it like any other expression.
            _ => match key.as_expression() {
                Some(expr) => self.lower_expression(expr),
                None => {
                    log::warn!("unsupported computed property key");
                    self.builder
                        .load_constant(crate::bytecode::Constant::String(std::sync::Arc::new(
                            "".into(),
                        )))
                }
            },
        }
    }

    /// `restObj = {}` minus the consumed keys, filled from `value`.
    fn emit_rest_object(&mut self, rest_obj: Value, value: Value, consumed: Vec<Value>) {
        let consumed_arr = self.builder.make_array();
        for k in &consumed {
            self.builder.array_push(consumed_arr, k.clone());
        }

        let obj_fn = self.builder.load_external_variable("Object".to_string());
        let keys = self
            .builder
            .call_property(obj_fn, "keys", vec![value.clone()]);
        let it = self.builder.make_iterator(keys);

        let cond_blk = self.create_block("rest_cond");
        let body_blk = self.create_block("rest_body");
        let put_blk = self.create_block("rest_put");
        let after_blk = self.create_block("rest_after");

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let (k, has_next) = self.builder.iterate_next(it);
        self.builder.br_if(has_next, body_blk, after_blk);

        self.builder.switch_to_block(body_blk);
        if consumed.is_empty() {
            let v = self.builder.index_get(value.clone(), k.clone());
            self.builder.index_set(rest_obj.clone(), k.clone(), v);
            self.builder.jump(cond_blk);
        } else {
            let taken = self
                .builder
                .call_property(consumed_arr, "includes", vec![k.clone()]);
            // taken -> skip (back to cond); not taken -> put and continue.
            self.builder.br_if(taken, cond_blk, put_blk);
            self.builder.switch_to_block(put_blk);
            let v = self.builder.index_get(value.clone(), k.clone());
            self.builder.index_set(rest_obj.clone(), k.clone(), v);
            self.builder.jump(cond_blk);
        }

        self.builder.switch_to_block(after_blk);
    }

    // ------------------- Destructuring assignment targets -------------------

    /// Bind an assignment target to `value` (`[a, b] = arr`, `({x} = obj)`).
    fn bind_assignment_target(&mut self, target: &AssignmentTarget<'_>, value: Value) {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(ident) => {
                self.store_into_identifier(ident.name.as_str(), value);
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                self.builder
                    .set_property(object, member.property.name.as_str(), value);
            }
            AssignmentTarget::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                self.builder.index_set(object, index, value);
            }
            AssignmentTarget::ArrayAssignmentTarget(target) => {
                self.bind_array_assignment_target(target, value);
            }
            AssignmentTarget::ObjectAssignmentTarget(target) => {
                self.bind_object_assignment_target(target, value);
            }
            _ => {
                log::warn!("unsupported assignment target in destructuring");
            }
        }
    }

    /// `[a, b, ...rest] = value`
    fn bind_array_assignment_target(
        &mut self,
        target: &ArrayAssignmentTarget<'_>,
        value: Value,
    ) {
        let it = self.builder.make_iterator(value);
        for element in &target.elements {
            match element {
                // Elision hole: step the iterator, discard the value.
                None => {
                    self.builder.iterate_next(it);
                }
                Some(element) => {
                    let (item, has_next) = self.builder.iterate_next(it);
                    self.bind_element_maybe_default(element, item, has_next);
                }
            }
        }
        if let Some(rest) = &target.rest {
            let rest_arr = self.builder.make_array();
            self.emit_rest_collect(rest_arr, it);
            self.bind_assignment_target(&rest.target, rest_arr);
        }
    }

    /// `({a, b: target, ...rest} = value)`
    fn bind_object_assignment_target(
        &mut self,
        target: &ObjectAssignmentTarget<'_>,
        value: Value,
    ) {
        let mut consumed: Vec<Value> = Vec::new();
        for prop in &target.properties {
            match prop {
                // Shorthand `{a}` — optionally `{a = default}`.
                AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                    let name = p.binding.name.to_string();
                    let kv = self.builder.load_constant(crate::bytecode::Constant::String(
                        std::sync::Arc::new(name.clone().into()),
                    ));
                    consumed.push(kv);
                    let prop_val = self.builder.get_property(value.clone(), &name);
                    match &p.init {
                        Some(init) => {
                            let result = self.builder.alloc();
                            self.builder.assign(result, prop_val);
                            let undefined = Value::Primitive(Primitive::Undefined);
                            let is_undef = self
                                .builder
                                .binop(Opcode::StrictEqual, result.clone(), undefined);
                            let default_val =
                                self.eval_default_on(&[is_undef], init, result.clone());
                            self.store_into_identifier(&name, default_val);
                        }
                        None => self.store_into_identifier(&name, prop_val),
                    }
                }
                // `{k: target}` (also `{"k": target}` and `{[k]: target}`).
                AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                    let (key_value, key_name, computed) =
                        self.lower_binding_property_key(&p.name, p.computed);
                    consumed.push(key_value.clone());
                    let prop_val = if computed {
                        self.builder.index_get(value.clone(), key_value)
                    } else {
                        self.builder
                            .get_property(value.clone(), key_name.unwrap_or_default().as_str())
                    };
                    match &p.binding {
                        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                            let result = self.builder.alloc();
                            self.builder.assign(result, prop_val);
                            let undefined = Value::Primitive(Primitive::Undefined);
                            let is_undef = self
                                .builder
                                .binop(Opcode::StrictEqual, result.clone(), undefined);
                            let default_val =
                                self.eval_default_on(&[is_undef], &d.init, result.clone());
                            self.bind_assignment_target(&d.binding, default_val);
                        }
                        other => self.bind_assignment_target_maybe(other, prop_val),
                    }
                }
            }
        }
        if let Some(rest) = &target.rest {
            let rest_obj = self.builder.make_object();
            self.emit_rest_object(rest_obj, value.clone(), consumed);
            self.bind_assignment_target(&rest.target, rest_obj);
        }
    }

    /// Bind one array element that may carry a default value.
    fn bind_element_maybe_default(
        &mut self,
        element: &AssignmentTargetMaybeDefault<'_>,
        item: Value,
        has_next: Value,
    ) {
        match element {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                // Default when the element is missing or explicitly undefined.
                let result = self.builder.alloc();
                self.builder.assign(result, item);
                let undefined = Value::Primitive(Primitive::Undefined);
                let is_undef = self
                    .builder
                    .binop(Opcode::StrictEqual, result.clone(), undefined);
                let no_next = self.builder.unaryop(Opcode::Not, has_next);
                let missing = self.builder.binop(Opcode::BitOr, no_next, is_undef);
                let default_val = self.eval_default_on(&[missing], &d.init, result.clone());
                self.bind_assignment_target(&d.binding, default_val);
            }
            other => self.bind_assignment_target_maybe(other, item),
        }
    }

    /// Same as `bind_assignment_target`, for the `MaybeDefault` wrapper.
    fn bind_assignment_target_maybe(
        &mut self,
        target: &AssignmentTargetMaybeDefault<'_>,
        value: Value,
    ) {
        match target {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                let result = self.builder.alloc();
                self.builder.assign(result, value);
                let undefined = Value::Primitive(Primitive::Undefined);
                let is_undef = self
                    .builder
                    .binop(Opcode::StrictEqual, result.clone(), undefined);
                let default_val = self.eval_default_on(&[is_undef], &d.init, result.clone());
                self.bind_assignment_target(&d.binding, default_val);
            }
            AssignmentTargetMaybeDefault::AssignmentTargetIdentifier(ident) => {
                self.store_into_identifier(ident.name.as_str(), value);
            }
            AssignmentTargetMaybeDefault::StaticMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                self.builder
                    .set_property(object, member.property.name.as_str(), value);
            }
            AssignmentTargetMaybeDefault::ComputedMemberExpression(member) => {
                let object = self.lower_expression(&member.object);
                let index = self.lower_expression(&member.expression);
                self.builder.index_set(object, index, value);
            }
            AssignmentTargetMaybeDefault::ArrayAssignmentTarget(target) => {
                self.bind_array_assignment_target(target, value);
            }
            AssignmentTargetMaybeDefault::ObjectAssignmentTarget(target) => {
                self.bind_object_assignment_target(target, value);
            }
            _ => {
                log::warn!("unsupported assignment target in destructuring");
            }
        }
    }

    /// Assign `value` to the identifier `name`, routing through the global
    /// environment when `name` is a script-scope global.
    fn store_into_identifier(&mut self, name: &str, value: Value) {
        if self.global_names.contains(name) {
            self.builder.store_external_variable(name.to_string(), value);
            return;
        }
        match self.symbols.lookup(name) {
            Some(var) => {
                self.builder.assign(var.0, value);
                self.sync_global(name, value);
            }
            None => self.builder.store_external_variable(name.to_string(), value),
        }
    }

    /// Copy every own enumerable key of `src` onto `target` (`{...src}`).
    fn emit_object_spread(&mut self, target: Value, src: Value) {
        let obj_fn = self.builder.load_external_variable("Object".to_string());
        let keys = self
            .builder
            .call_property(obj_fn, "keys", vec![src.clone()]);
        let it = self.builder.make_iterator(keys);

        let cond_blk = self.create_block("ospr_cond");
        let body_blk = self.create_block("ospr_body");
        let after_blk = self.create_block("ospr_after");

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let (k, has_next) = self.builder.iterate_next(it);
        self.builder.br_if(has_next, body_blk, after_blk);

        self.builder.switch_to_block(body_blk);
        let v = self.builder.index_get(src.clone(), k.clone());
        self.builder.index_set(target.clone(), k, v);
        self.builder.jump(cond_blk);

        self.builder.switch_to_block(after_blk);
    }

    /// Collect the remaining iterator elements into `rest_arr`.
    fn emit_rest_collect(&mut self, rest_arr: Value, it: Value) {
        let cond_blk = self.create_block("restcol_cond");
        let body_blk = self.create_block("restcol_body");
        let after_blk = self.create_block("restcol_after");

        self.builder.jump(cond_blk);
        self.builder.switch_to_block(cond_blk);
        let (item, has_next) = self.builder.iterate_next(it);
        self.builder.br_if(has_next, body_blk, after_blk);

        self.builder.switch_to_block(body_blk);
        self.builder.array_push(rest_arr, item);
        self.builder.jump(cond_blk);

        self.builder.switch_to_block(after_blk);
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
