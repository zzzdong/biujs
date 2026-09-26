use std::collections::HashMap;

/// Symbol table with scope chain for variable lookup
#[derive(Debug)]
pub struct SymbolTable<T> {
    scopes: Vec<Scope<T>>,
}

impl<T> SymbolTable<T> {
    pub fn new() -> Self {
        SymbolTable::<T> {
            scopes: vec![Scope::<T>::new()],
        }
    }

    pub fn lookup(&self, name: impl AsRef<str>) -> Option<&T> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.variables.get(name.as_ref()) {
                return Some(ty);
            }
        }
        None
    }

    pub fn insert(&mut self, name: impl Into<String>, value: T) {
        self.scopes
            .last_mut()
            .unwrap()
            .variables
            .insert(name.into(), value);
    }

    /// Insert into a specific scope, counting from the outermost.
    ///
    /// `var` bindings belong to the enclosing *function* (or the script), not to
    /// the block they appear in, so a declaration inside `{ … }` has to be
    /// recorded in a scope that outlives the block.
    pub fn insert_at(&mut self, scope_index: usize, name: impl Into<String>, value: T) {
        if let Some(scope) = self.scopes.get_mut(scope_index) {
            scope.variables.insert(name.into(), value);
        }
    }

    /// Scope index the name resolves in (innermost wins), or `None`.
    pub fn lookup_depth(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rposition(|scope| scope.variables.contains_key(name))
    }

    /// Look a name up in one specific scope, counting from the outermost.
    ///
    /// Used for `var`: the binding belongs to the function scope and must be
    /// found there even while an inner block is open (and even when an unrelated
    /// block-local `let` shares the name).
    pub fn lookup_at(&self, scope_index: usize, name: &str) -> Option<&T> {
        self.scopes.get(scope_index).and_then(|s| s.variables.get(name))
    }

    /// Number of nested scopes currently open.
    pub fn scope_count(&self) -> usize {
        self.scopes.len()
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(Scope::<T>::new());
    }

    pub fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn remove(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.variables.remove(name).is_some() {
                return;
            }
        }
    }
}

impl SymbolTable<crate::compiler::lowering::Variable> {
    /// Mark the binding of `name` as initialized (end of its temporal dead zone).
    pub fn mark_initialized(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.variables.get_mut(name) {
                binding.initialized = true;
                return;
            }
        }
    }
}

impl<T: Clone> Clone for SymbolTable<T> {
    fn clone(&self) -> Self {
        SymbolTable {
            scopes: self.scopes.clone(),
        }
    }
}

#[derive(Debug)]
struct Scope<T> {
    variables: HashMap<String, T>,
}

impl<T> Scope<T> {
    fn new() -> Self {
        Scope {
            variables: HashMap::new(),
        }
    }
}

impl<T: Clone> Clone for Scope<T> {
    fn clone(&self) -> Self {
        Scope {
            variables: self.variables.clone(),
        }
    }
}
