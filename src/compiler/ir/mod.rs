pub mod builder;
pub mod cfg;
pub mod instruction;
pub mod ssabuilder;

use std::fmt;

pub use builder::{FunctionBuilder, InstBuilder, IrBuilder};
pub use cfg::{Block, ControlFlowGraph};
pub use instruction::{BlockId, Instruction, Name, Value, Variable};
pub use ssabuilder::SSABuilder;

use crate::bytecode::{Constant, ConstantId, FunctionId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncParam {
    pub name: Name,
}
impl FuncParam {
    pub fn new(name: impl Into<Name>) -> Self {
        Self { name: name.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSignature {
    pub name: Name,
    pub params: Vec<FuncParam>,
    /// `ExpectedArgumentCount` (ES 9.2.4): the number of parameters before the
    /// first one with an initializer or rest element. Exposed as `fn.length`,
    /// which is *not* the same as `params.len()`.
    pub arity: usize,
    /// `function*`. A call to such a function does not push a frame: it builds
    /// a generator object whose body runs one `yield` at a time (ES 25.4).
    pub is_generator: bool,
    /// Constructor of a class with an `extends` clause (ES 9.2.2
    /// `[[ConstructorKind]] = derived`): its `this` is uninitialized until
    /// `super()` binds it.
    pub is_derived_ctor: bool,
}
impl FuncSignature {
    pub fn new(name: impl Into<Name>, params: Vec<FuncParam>) -> Self {
        let arity = params.len();
        Self {
            name: name.into(),
            params,
            arity,
            is_generator: false,
            is_derived_ctor: false,
        }
    }

    /// Same as [`FuncSignature::new`], but with an explicit `fn.length`.
    pub fn with_arity(name: impl Into<Name>, params: Vec<FuncParam>, arity: usize) -> Self {
        Self {
            name: name.into(),
            params,
            arity,
            is_generator: false,
            is_derived_ctor: false,
        }
    }

    /// Mark the function as a generator (`function*`).
    pub fn as_generator(mut self) -> Self {
        self.is_generator = true;
        self
    }

    /// Mark the function as a derived-class constructor.
    pub fn as_derived_ctor(mut self) -> Self {
        self.is_derived_ctor = true;
        self
    }
}

impl fmt::Display for FuncSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.params.is_empty() {
            write!(f, "(")?;
            for (i, param) in self.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", param.name)?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct IrFunction {
    pub id: FunctionId,
    pub signature: FuncSignature,
    pub control_flow_graph: ControlFlowGraph,
}

impl IrFunction {
    pub fn new(id: FunctionId, signature: FuncSignature) -> Self {
        Self {
            id,
            signature,
            control_flow_graph: ControlFlowGraph::new(),
        }
    }
}

impl fmt::Display for IrFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IrFunction")?;
        writeln!(f)?;

        writeln!(f, "=== signature ===")?;
        writeln!(f, "{}", self.signature)?;

        writeln!(f, "=== control flow graph ===")?;
        writeln!(f, "{}", self.control_flow_graph)?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct IrUnit {
    pub constants: Vec<Constant>,
    pub functions: Vec<IrFunction>,
    pub control_flow_graph: ControlFlowGraph,
}

impl IrUnit {
    pub fn new() -> Self {
        Self {
            constants: Vec::new(),
            functions: Vec::new(),
            control_flow_graph: ControlFlowGraph::new(),
        }
    }

    pub fn get_function(&self, id: FunctionId) -> Option<&IrFunction> {
        self.functions.get(id.as_usize())
    }

    pub fn get_function_mut(&mut self, id: FunctionId) -> Option<&mut IrFunction> {
        self.functions.get_mut(id.as_usize())
    }

    pub fn find_function(&self, name: &str) -> Option<&IrFunction> {
        self.functions.iter().find(|f| match f.signature.name.0 {
            Some(ref n) => n == name,
            None => false,
        })
    }

    pub fn make_constant(&mut self, value: Constant) -> ConstantId {
        match self.constants.iter().position(|item| item == &value) {
            Some(index) => ConstantId::new(index as u32),
            None => {
                let id = ConstantId::new(self.constants.len() as u32);
                self.constants.push(value);
                id
            }
        }
    }

    /// Declare a fresh function, returning its (unique) id.
    ///
    /// Ids are intentionally **not** de-duplicated by signature: two distinct
    /// function expressions may well share a name and parameter list (e.g. the
    /// anonymous `(actual, expected, message) => …` helpers in test262's
    /// `assert.js`). Re-using an id would make the second definition overwrite
    /// the first one's body.
    pub fn declare_function(&mut self, signature: FuncSignature) -> FunctionId {
        let id = FunctionId::new(self.functions.len() as u32);
        self.functions.push(IrFunction::new(id, signature));
        id
    }

    pub fn define_function(&mut self, id: FunctionId, func: IrFunction) {
        let old = &mut self.functions[id.as_usize()];
        let _ = std::mem::replace(old, func);
    }

    pub fn get_block(&self, id: BlockId) -> Option<&Block> {
        self.control_flow_graph.get_block(id)
    }

    pub fn with_control_flow_graph(mut self, control_flow_graph: ControlFlowGraph) -> Self {
        self.control_flow_graph = control_flow_graph;
        self
    }
}

impl fmt::Display for IrUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IrUnit")?;
        writeln!(f)?;

        writeln!(f, "=== constants ===")?;
        for (i, constant) in self.constants.iter().enumerate() {
            writeln!(f, "{i}\t: {constant}")?;
        }

        writeln!(f, "=== functions ===")?;
        for (i, function) in self.functions.iter().enumerate() {
            writeln!(f, "{i}\t: {function}")?;
        }

        writeln!(f, "=== control flow graph ===")?;
        writeln!(f, "{}", self.control_flow_graph)?;

        Ok(())
    }
}
