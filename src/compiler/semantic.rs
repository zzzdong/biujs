//! Semantic analyzer for JavaScript AST.
//!
//! Performs semantic checks before lowering to IR:
//! - const variable reassignment detection
//! - (future) undefined variable detection
//! - (future) duplicate declaration detection

use oxc_ast::ast::*;
use oxc_span::Span;
use std::collections::HashMap;

use crate::compiler::error::{CompileError, SourceLocation};

/// Variable binding information
#[derive(Debug, Clone, Copy, PartialEq)]
enum BindingKind {
    Let,
    Const,
    Function,
}

/// A scope containing variable bindings
struct Scope {
    bindings: HashMap<String, BindingKind>,
}

impl Scope {
    fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, name: String, kind: BindingKind) {
        self.bindings.insert(name, kind);
    }

    fn lookup(&self, name: &str) -> Option<BindingKind> {
        self.bindings.get(name).copied()
    }
}

/// Semantic analyzer for JavaScript code
pub struct SemanticAnalyzer {
    scopes: Vec<Scope>,
    errors: Vec<CompileError>,
}

impl SemanticAnalyzer {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope::new()],
            errors: Vec::new(),
        }
    }

    /// Analyze a JavaScript program and return any semantic errors
    pub fn analyze(&mut self, program: &Program<'_>) -> Vec<CompileError> {
        self.visit_program(program);
        std::mem::take(&mut self.errors)
    }

    fn enter_scope(&mut self) {
        self.scopes.push(Scope::new());
    }

    fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup_binding(&self, name: &str) -> Option<BindingKind> {
        for scope in self.scopes.iter().rev() {
            if let Some(kind) = scope.lookup(name) {
                return Some(kind);
            }
        }
        None
    }

    fn current_scope(&mut self) -> &mut Scope {
        self.scopes.last_mut().unwrap()
    }

    fn add_error(&mut self, message: impl Into<String>, span: Span) {
        let location = self.span_to_location(span);
        self.errors.push(CompileError::SemanticError {
            message: message.into(),
            location: Some(location),
        });
    }

    fn span_to_location(&self, span: Span) -> SourceLocation {
        // For now, use byte offset as approximation
        SourceLocation::new(1, span.start)
    }

    fn visit_program(&mut self, program: &Program<'_>) {
        for stmt in &program.body {
            self.visit_statement(stmt);
        }
    }

    fn visit_statement(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::BlockStatement(block) => {
                self.enter_scope();
                for stmt in &block.body {
                    self.visit_statement(stmt);
                }
                self.leave_scope();
            }
            Statement::VariableDeclaration(decl) => {
                let kind = match decl.kind {
                    VariableDeclarationKind::Const => BindingKind::Const,
                    VariableDeclarationKind::Let => BindingKind::Let,
                    _ => BindingKind::Let,
                };

                for declarator in &decl.declarations {
                    self.collect_binding_names(&declarator.id, kind);
                    if let Some(init) = &declarator.init {
                        self.visit_expression(init);
                    }
                }
            }
            Statement::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    self.current_scope()
                        .insert(id.name.to_string(), BindingKind::Function);
                }

                self.enter_scope();

                if let Some(body) = &func.body {
                    for stmt in &body.statements {
                        self.visit_statement(stmt);
                    }
                }

                self.leave_scope();
            }
            Statement::IfStatement(if_stmt) => {
                self.visit_expression(&if_stmt.test);
                self.visit_statement(&if_stmt.consequent);
                if let Some(alt) = &if_stmt.alternate {
                    self.visit_statement(alt);
                }
            }
            Statement::WhileStatement(while_stmt) => {
                self.visit_expression(&while_stmt.test);
                self.visit_statement(&while_stmt.body);
            }
            Statement::ForStatement(for_stmt) => {
                self.enter_scope();

                if let Some(init) = &for_stmt.init {
                    if let ForStatementInit::VariableDeclaration(decl) = init {
                        let kind = match decl.kind {
                            VariableDeclarationKind::Const => BindingKind::Const,
                            VariableDeclarationKind::Let => BindingKind::Let,
                            _ => BindingKind::Let,
                        };
                        for declarator in &decl.declarations {
                            self.collect_binding_names(&declarator.id, kind);
                            if let Some(init) = &declarator.init {
                                self.visit_expression(init);
                            }
                        }
                    }
                }

                if let Some(test) = &for_stmt.test {
                    self.visit_expression(test);
                }
                if let Some(update) = &for_stmt.update {
                    self.visit_expression(update);
                }

                self.visit_statement(&for_stmt.body);
                self.leave_scope();
            }
            Statement::TryStatement(try_stmt) => {
                // Try block
                self.enter_scope();
                for stmt in &try_stmt.block.body {
                    self.visit_statement(stmt);
                }
                self.leave_scope();

                if let Some(catch) = &try_stmt.handler {
                    self.enter_scope();
                    for stmt in &catch.body.body {
                        self.visit_statement(stmt);
                    }
                    self.leave_scope();
                }

                if let Some(finally) = &try_stmt.finalizer {
                    self.enter_scope();
                    for stmt in &finally.body {
                        self.visit_statement(stmt);
                    }
                    self.leave_scope();
                }
            }
            Statement::ReturnStatement(ret) => {
                if let Some(arg) = &ret.argument {
                    self.visit_expression(arg);
                }
            }
            Statement::ThrowStatement(throw) => {
                self.visit_expression(&throw.argument);
            }
            Statement::ExpressionStatement(expr) => {
                self.visit_expression(&expr.expression);
            }
            _ => {}
        }
    }

    fn visit_expression(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::AssignmentExpression(assign) => {
                // Check for const reassignment
                self.check_assignment_target(&assign.left, assign.span);
                self.visit_expression(&assign.right);
            }
            Expression::UpdateExpression(update) => {
                // Check for const update
                self.check_update_target(&update.argument, update.span);
            }
            Expression::BinaryExpression(bin) => {
                self.visit_expression(&bin.left);
                self.visit_expression(&bin.right);
            }
            Expression::UnaryExpression(unary) => {
                self.visit_expression(&unary.argument);
            }
            Expression::CallExpression(call) => {
                self.visit_expression(&call.callee);
            }
            // MemberExpression variants are handled through the inherit_variants! macro
            // They include StaticMemberExpression and ComputedMemberExpression
            _ => {
                // For other expression types, we don't need to recurse for const checking
            }
            Expression::ArrayExpression(_) => {
                // Simplified: skip array elements for now
            }
            Expression::ObjectExpression(obj) => {
                for prop in &obj.properties {
                    if let ObjectPropertyKind::ObjectProperty(prop) = prop {
                        self.visit_expression(&prop.value);
                    }
                }
            }
            _ => {}
        }
    }

    /// Check if an assignment target is a const variable
    fn check_assignment_target(&mut self, target: &AssignmentTarget<'_>, span: Span) {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(ident) => {
                if let Some(kind) = self.lookup_binding(&ident.name) {
                    if kind == BindingKind::Const {
                        self.add_error(
                            format!(
                                "TypeError: Assignment to constant variable '{}'",
                                ident.name
                            ),
                            span,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Check if an update expression target is a const variable
    fn check_update_target(&mut self, target: &SimpleAssignmentTarget<'_>, span: Span) {
        match target {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
                if let Some(kind) = self.lookup_binding(&ident.name) {
                    if kind == BindingKind::Const {
                        self.add_error(
                            format!(
                                "TypeError: Assignment to constant variable '{}'",
                                ident.name
                            ),
                            span,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Collect all binding names from a binding pattern
    fn collect_binding_names(&mut self, pattern: &BindingPattern<'_>, kind: BindingKind) {
        if let Some(name) = pattern.get_identifier_name() {
            self.current_scope().insert(name.to_string(), kind);
        }
        // Complex patterns (destructuring) are handled at runtime for now
    }
}
