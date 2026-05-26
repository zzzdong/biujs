use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

pub const MIN_REQUIRED_REGISTER: usize = 3;

#[derive(Debug, Clone)]
pub struct Module {
    pub name: Option<String>,
    pub constants: Vec<Constant>,
    pub symtab: HashMap<FunctionId, usize>,
    pub instructions: Vec<Bytecode>,
    pub debug_instructions: BTreeMap<usize, crate::compiler::ir::Instruction>,
}

impl Module {
    pub fn new(
        name: impl Into<Option<String>>,
        constants: Vec<Constant>,
        symtab: HashMap<FunctionId, usize>,
        instructions: Vec<Bytecode>,
    ) -> Self {
        Self {
            name: name.into(),
            constants,
            symtab,
            instructions,
            debug_instructions: BTreeMap::new(),
        }
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Module")?;
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        writeln!(f)?;

        writeln!(f, "=== constants ===")?;
        for (i, constant) in self.constants.iter().enumerate() {
            writeln!(f, "{i}\t: {constant}")?;
        }

        writeln!(f, "=== symtab ===")?;
        for (function_id, &index) in &self.symtab {
            writeln!(f, "{function_id}: {index}")?;
        }

        writeln!(f, "=== instructions ===")?;
        for (i, instruction) in self.instructions.iter().enumerate() {
            write!(f, "{i}\t: {instruction}")?;
            if let Some(debug_instruction) = self.debug_instructions.get(&i) {
                writeln!(f, "\t;{debug_instruction:<16}")?;
            } else {
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Bytecode {
    pub opcode: Opcode,
    pub operands: [Operand; 3],
}

impl Bytecode {
    pub fn empty(opcode: Opcode) -> Self {
        Self {
            opcode,
            operands: [Operand::Immd(0); 3],
        }
    }

    pub fn single(opcode: Opcode, operand: Operand) -> Self {
        Self {
            opcode,
            operands: [operand, Operand::Immd(0), Operand::Immd(0)],
        }
    }

    pub fn double(opcode: Opcode, dst: Operand, src: Operand) -> Self {
        Self {
            opcode,
            operands: [dst, src, Operand::Immd(0)],
        }
    }

    pub fn triple(opcode: Opcode, dst: Operand, src1: Operand, src2: Operand) -> Self {
        Self {
            opcode,
            operands: [dst, src1, src2],
        }
    }
}

impl fmt::Display for Bytecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.opcode)?;
        if let Some((last, operands)) = self.operands.split_last() {
            for operand in operands {
                write!(f, " {operand},")?;
            }
            write!(f, " {last}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Opcode {
    /// load_const dst, const_id
    LoadConst,
    /// load_env dst, name
    LoadEnv,
    /// halt
    Halt,
    /// push src
    Push,
    /// pop dst
    Pop,
    /// pushc offset
    PushC,
    /// popc dst
    PopC,
    /// addc offset
    AddC,
    /// subc offset
    SubC,
    /// movc
    MovC,
    /// call func_id
    Call,
    /// call_ex callable
    CallEx,
    /// call_native callable args_count
    CallNative,
    /// ret value
    Ret,
    /// mov dst, src
    Mov,
    /// jump offset
    Jump,
    /// br_if cond, true_offset, false_offset
    BrIf,
    /// not dst, src
    Not,
    /// bitnot dst, src (bitwise NOT ~)
    BitNot,
    /// neg dst, src
    Neg,
    /// addx dst, src1, src2 (object addition)
    Addx,
    /// subx dst, src1, src2 (object subtraction)
    Subx,
    /// mulx dst, src1, src2 (object multiplication)
    Mulx,
    /// divx dst, src1, src2 (object division)
    Divx,
    /// remx dst, src1, src2 (object remainder)
    Remx,
    /// and dst, src1, src2
    And,
    /// or dst, src1, src2
    Or,
    /// less dst, src1, src2
    Less,
    /// less_equal dst, src1, src2
    LessEqual,
    /// greater dst, src1, src2
    Greater,
    /// greater_equal dst, src1, src2
    GreaterEqual,
    /// equal dst, src1, src2
    Equal,
    /// not_equal dst, src1, src2
    NotEqual,
    /// strict_equal dst, src1, src2 (JS ===)
    StrictEqual,
    /// strict_not_equal dst, src1, src2 (JS !==)
    StrictNotEqual,
    /// typeof dst, src
    TypeOf,
    /// instanceof dst, src1, src2
    InstanceOf,
    /// in dst, src1, src2
    In,
    /// make_iter dst, src
    MakeIter,
    /// iter_next dst, has_next, src
    IterNext,
    /// make_array dst
    MakeArray,
    /// array_push dst, src
    ArrayPush,
    /// make_object dst
    MakeObject,
    /// index_get dst, obj, idx
    IndexGet,
    /// index_set obj, idx, value
    IndexSet,
    /// prop_get dst, obj, prop
    PropGet,
    /// prop_set obj, prop, value
    PropSet,
    /// call_method dst, obj, method
    CallMethod,
    /// try catch_offset, finally_offset (SEH: register handler at offset)
    Try,
    /// end_try (SEH: unregister handler)
    EndTry,
    /// throw src (SEH: throw exception)
    ThrowExc,
    /// load_exception dst (SEH: load caught exception)
    LoadException,
    /// resume_exception (SEH: re-throw pending exception after finally)
    ResumeExc,
    /// delayed_jump target, seh_depth (execute finally blocks then jump)
    DelayedJump,
    /// create_closure dst, func_id
    CreateClosure,
    /// new dst, constructor
    New,
    /// load_this dst
    LoadThis,
    /// make_func_obj dst, func_id
    MakeFuncObj,
    /// make_arrow_func_obj dst, func_id, captured_this
    MakeArrowFuncObj,
    /// closure_var name, value — push a captured variable for the next MakeArrowFuncObj
    ClosureVar,
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Opcode::Jump => write!(f, "br"),
            Opcode::BrIf => write!(f, "br_if"),
            Opcode::Halt => write!(f, "halt"),
            Opcode::Push => write!(f, "push"),
            Opcode::Pop => write!(f, "pop"),
            Opcode::PushC => write!(f, "pushc"),
            Opcode::PopC => write!(f, "popc"),
            Opcode::MovC => write!(f, "movc"),
            Opcode::AddC => write!(f, "addc"),
            Opcode::SubC => write!(f, "subc"),
            Opcode::Call => write!(f, "call"),
            Opcode::CallEx => write!(f, "call_ex"),
            Opcode::CallNative => write!(f, "call_native"),
            Opcode::Ret => write!(f, "ret"),
            Opcode::LoadConst => write!(f, "load_const"),
            Opcode::LoadEnv => write!(f, "load_env"),
            Opcode::Mov => write!(f, "mov"),
            Opcode::Not => write!(f, "not"),
            Opcode::BitNot => write!(f, "bitnot"),
            Opcode::Neg => write!(f, "neg"),
            Opcode::Addx => write!(f, "addx"),
            Opcode::Subx => write!(f, "subx"),
            Opcode::Mulx => write!(f, "mulx"),
            Opcode::Divx => write!(f, "divx"),
            Opcode::Remx => write!(f, "remx"),
            Opcode::And => write!(f, "and"),
            Opcode::Or => write!(f, "or"),
            Opcode::Less => write!(f, "lt"),
            Opcode::LessEqual => write!(f, "lte"),
            Opcode::Greater => write!(f, "gt"),
            Opcode::GreaterEqual => write!(f, "gte"),
            Opcode::Equal => write!(f, "eq"),
            Opcode::NotEqual => write!(f, "ne"),
            Opcode::StrictEqual => write!(f, "seq"),
            Opcode::StrictNotEqual => write!(f, "sne"),
            Opcode::TypeOf => write!(f, "typeof"),
            Opcode::InstanceOf => write!(f, "instanceof"),
            Opcode::In => write!(f, "in"),
            Opcode::MakeIter => write!(f, "make_iter"),
            Opcode::IterNext => write!(f, "iter_next"),
            Opcode::MakeArray => write!(f, "make_array"),
            Opcode::ArrayPush => write!(f, "array_push"),
            Opcode::MakeObject => write!(f, "make_object"),
            Opcode::IndexGet => write!(f, "index_get"),
            Opcode::IndexSet => write!(f, "index_set"),
            Opcode::PropGet => write!(f, "prop_get"),
            Opcode::PropSet => write!(f, "prop_set"),
            Opcode::CallMethod => write!(f, "call_method"),
            Opcode::Try => write!(f, "try"),
            Opcode::EndTry => write!(f, "end_try"),
            Opcode::ThrowExc => write!(f, "throw"),
            Opcode::LoadException => write!(f, "load_exception"),
            Opcode::ResumeExc => write!(f, "resume_exception"),
            Opcode::DelayedJump => write!(f, "delayed_jump"),
            Opcode::CreateClosure => write!(f, "create_closure"),
            Opcode::New => write!(f, "new"),
            Opcode::LoadThis => write!(f, "load_this"),
            Opcode::MakeFuncObj => write!(f, "make_func_obj"),
            Opcode::MakeArrowFuncObj => write!(f, "make_arrow_func_obj"),
            Opcode::ClosureVar => write!(f, "closure_var"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Hash)]
pub enum Operand {
    Primitive(Primitive),
    Register(Register),
    Stack(isize),
    Immd(isize),
    Symbol(u32),
}

impl Operand {
    pub fn new_immd(immd: isize) -> Self {
        Self::Immd(immd)
    }

    pub fn new_primitive(primitive: Primitive) -> Self {
        Self::Primitive(primitive)
    }

    pub fn new_register(reg: Register) -> Self {
        Self::Register(reg)
    }

    pub fn new_stack(offset: isize) -> Self {
        Self::Stack(offset)
    }

    pub fn new_symbol(id: u32) -> Self {
        Self::Symbol(id)
    }

    pub fn as_immd(&self) -> isize {
        match self {
            Operand::Immd(immd) => *immd,
            _ => panic!("{self:?} not an immediate"),
        }
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Primitive(primitive) => write!(f, "{primitive}"),
            Operand::Register(reg) => write!(f, "{reg}"),
            Operand::Stack(offset) => write!(f, "[rbp{offset:+}]"),
            Operand::Immd(immd) => write!(f, "{immd}"),
            Operand::Symbol(id) => write!(f, "sym_{id}"),
        }
    }
}

impl From<Register> for Operand {
    fn from(reg: Register) -> Self {
        Operand::Register(reg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Hash)]
pub enum Register {
    R0 = 0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    /// Stack pointer
    Rsp,
    /// Base pointer
    Rbp,
    /// Return value
    Rv,
}

impl Register {
    pub fn as_usize(&self) -> usize {
        *self as usize
    }

    pub fn general() -> [Register; 16] {
        [
            Register::R0,
            Register::R1,
            Register::R2,
            Register::R3,
            Register::R4,
            Register::R5,
            Register::R6,
            Register::R7,
            Register::R8,
            Register::R9,
            Register::R10,
            Register::R11,
            Register::R12,
            Register::R13,
            Register::R14,
            Register::R15,
        ]
    }

    pub fn small_general() -> [Register; 4] {
        [Register::R0, Register::R1, Register::R2, Register::R3]
    }

    pub fn all() -> [Register; 19] {
        [
            Register::R0,
            Register::R1,
            Register::R2,
            Register::R3,
            Register::R4,
            Register::R5,
            Register::R6,
            Register::R7,
            Register::R8,
            Register::R9,
            Register::R10,
            Register::R11,
            Register::R12,
            Register::R13,
            Register::R14,
            Register::R15,
            Register::Rsp,
            Register::Rbp,
            Register::Rv,
        ]
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Register::R0 => write!(f, "r0"),
            Register::R1 => write!(f, "r1"),
            Register::R2 => write!(f, "r2"),
            Register::R3 => write!(f, "r3"),
            Register::R4 => write!(f, "r4"),
            Register::R5 => write!(f, "r5"),
            Register::R6 => write!(f, "r6"),
            Register::R7 => write!(f, "r7"),
            Register::R8 => write!(f, "r8"),
            Register::R9 => write!(f, "r9"),
            Register::R10 => write!(f, "r10"),
            Register::R11 => write!(f, "r11"),
            Register::R12 => write!(f, "r12"),
            Register::R13 => write!(f, "r13"),
            Register::R14 => write!(f, "r14"),
            Register::R15 => write!(f, "r15"),
            Register::Rsp => write!(f, "rsp"),
            Register::Rbp => write!(f, "rbp"),
            Register::Rv => write!(f, "rv"),
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Primitive {
    #[default]
    Null,
    Undefined,
    Boolean(bool),
    Integer(i64),
    Float(f64),
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Primitive::Null => write!(f, "null"),
            Primitive::Undefined => write!(f, "undefined"),
            Primitive::Boolean(b) => write!(f, "{b}"),
            Primitive::Integer(i) => write!(f, "{i}"),
            Primitive::Float(ff) => write!(f, "{ff}"),
        }
    }
}

impl From<bool> for Primitive {
    fn from(value: bool) -> Self {
        Primitive::Boolean(value)
    }
}

impl From<i64> for Primitive {
    fn from(value: i64) -> Self {
        Primitive::Integer(value)
    }
}

impl From<f64> for Primitive {
    fn from(value: f64) -> Self {
        Primitive::Float(value)
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Hash)]
pub enum Constant {
    String(Arc<String>),
}

impl From<&str> for Constant {
    fn from(value: &str) -> Self {
        Constant::String(Arc::new(value.to_string()))
    }
}

impl From<String> for Constant {
    fn from(value: String) -> Self {
        Constant::String(Arc::new(value.to_string()))
    }
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::String(s) => write!(f, "\"{s}\""),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct ConstantId(u32);

impl ConstantId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    pub fn as_isize(&self) -> isize {
        self.0 as isize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct FunctionId(u32);

impl FunctionId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    pub fn as_isize(&self) -> isize {
        self.0 as isize
    }
}

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──────────────────────── Operand ────────────────────────

    #[test]
    fn test_operand_immd() {
        let op = Operand::new_immd(42);
        assert_eq!(op.as_immd(), 42);
    }

    #[test]
    fn test_operand_register() {
        let op = Operand::new_register(Register::R0);
        match op {
            Operand::Register(reg) => assert_eq!(reg, Register::R0),
            _ => panic!("expected Register"),
        }
    }

    #[test]
    fn test_operand_primitive() {
        let op = Operand::new_primitive(Primitive::Null);
        match op {
            Operand::Primitive(p) => assert_eq!(p, Primitive::Null),
            _ => panic!("expected Primitive"),
        }
    }

    #[test]
    fn test_operand_stack() {
        let op = Operand::new_stack(-8);
        match op {
            Operand::Stack(offset) => assert_eq!(offset, -8),
            _ => panic!("expected Stack"),
        }
    }

    #[test]
    fn test_operand_symbol() {
        let op = Operand::new_symbol(5);
        match op {
            Operand::Symbol(id) => assert_eq!(id, 5),
            _ => panic!("expected Symbol"),
        }
    }

    #[test]
    fn test_operand_display() {
        assert_eq!(format!("{}", Operand::new_immd(42)), "42");
        assert_eq!(format!("{}", Operand::new_register(Register::R0)), "r0");
        assert_eq!(format!("{}", Operand::new_primitive(Primitive::Null)), "null");
        assert_eq!(format!("{}", Operand::new_stack(-8)), "[rbp-8]");
        assert_eq!(format!("{}", Operand::new_symbol(5)), "sym_5");
    }

    // ──────────────────────── Register ────────────────────────

    #[test]
    fn test_register_as_usize() {
        assert_eq!(Register::R0.as_usize(), 0);
        assert_eq!(Register::R15.as_usize(), 15);
        assert_eq!(Register::Rsp.as_usize(), 16);
        assert_eq!(Register::Rbp.as_usize(), 17);
        assert_eq!(Register::Rv.as_usize(), 18);
    }

    #[test]
    fn test_register_general() {
        let regs = Register::general();
        assert_eq!(regs.len(), 16);
        assert_eq!(regs[0], Register::R0);
        assert_eq!(regs[15], Register::R15);
    }

    #[test]
    fn test_register_small_general() {
        let regs = Register::small_general();
        assert_eq!(regs.len(), 4);
        assert_eq!(regs[0], Register::R0);
        assert_eq!(regs[3], Register::R3);
    }

    #[test]
    fn test_register_all() {
        let regs = Register::all();
        assert_eq!(regs.len(), 19);
        assert_eq!(regs[0], Register::R0);
        assert_eq!(regs[16], Register::Rsp);
        assert_eq!(regs[17], Register::Rbp);
        assert_eq!(regs[18], Register::Rv);
    }

    #[test]
    fn test_register_display() {
        assert_eq!(format!("{}", Register::R0), "r0");
        assert_eq!(format!("{}", Register::R15), "r15");
        assert_eq!(format!("{}", Register::Rsp), "rsp");
        assert_eq!(format!("{}", Register::Rbp), "rbp");
        assert_eq!(format!("{}", Register::Rv), "rv");
    }

    // ──────────────────────── Primitive ────────────────────────

    #[test]
    fn test_primitive_default() {
        assert_eq!(Primitive::default(), Primitive::Null);
    }

    #[test]
    fn test_primitive_display() {
        assert_eq!(format!("{}", Primitive::Null), "null");
        assert_eq!(format!("{}", Primitive::Undefined), "undefined");
        assert_eq!(format!("{}", Primitive::Boolean(true)), "true");
        assert_eq!(format!("{}", Primitive::Boolean(false)), "false");
        assert_eq!(format!("{}", Primitive::Integer(42)), "42");
        assert_eq!(format!("{}", Primitive::Float(3.14)), "3.14");
    }

    #[test]
    fn test_primitive_from_bool() {
        assert_eq!(Primitive::from(true), Primitive::Boolean(true));
        assert_eq!(Primitive::from(false), Primitive::Boolean(false));
    }

    #[test]
    fn test_primitive_from_i64() {
        assert_eq!(Primitive::from(42i64), Primitive::Integer(42));
        assert_eq!(Primitive::from(-1i64), Primitive::Integer(-1));
    }

    #[test]
    fn test_primitive_from_f64() {
        assert_eq!(Primitive::from(3.14f64), Primitive::Float(3.14));
        assert_eq!(Primitive::from(0.0f64), Primitive::Float(0.0));
    }

    // ──────────────────────── Bytecode ────────────────────────

    #[test]
    fn test_bytecode_empty() {
        let inst = Bytecode::empty(Opcode::Halt);
        assert!(matches!(inst.opcode, Opcode::Halt));
        for op in &inst.operands {
            assert!(matches!(op, Operand::Immd(0)));
        }
    }

    #[test]
    fn test_bytecode_single() {
        let inst = Bytecode::single(Opcode::Push, Operand::Register(Register::R0));
        assert!(matches!(inst.opcode, Opcode::Push));
        assert!(matches!(inst.operands[0], Operand::Register(Register::R0)));
        assert!(matches!(inst.operands[1], Operand::Immd(0)));
        assert!(matches!(inst.operands[2], Operand::Immd(0)));
    }

    #[test]
    fn test_bytecode_double() {
        let inst = Bytecode::double(
            Opcode::Mov,
            Operand::Register(Register::R0),
            Operand::Register(Register::R1),
        );
        assert!(matches!(inst.opcode, Opcode::Mov));
        assert!(matches!(inst.operands[0], Operand::Register(Register::R0)));
        assert!(matches!(inst.operands[1], Operand::Register(Register::R1)));
        assert!(matches!(inst.operands[2], Operand::Immd(0)));
    }

    #[test]
    fn test_bytecode_triple() {
        let inst = Bytecode::triple(
            Opcode::Addx,
            Operand::Register(Register::R0),
            Operand::Register(Register::R1),
            Operand::Register(Register::R2),
        );
        assert!(matches!(inst.opcode, Opcode::Addx));
        assert!(matches!(inst.operands[0], Operand::Register(Register::R0)));
        assert!(matches!(inst.operands[1], Operand::Register(Register::R1)));
        assert!(matches!(inst.operands[2], Operand::Register(Register::R2)));
    }

    #[test]
    fn test_bytecode_display() {
        let inst = Bytecode::triple(
            Opcode::Addx,
            Operand::Register(Register::R0),
            Operand::Register(Register::R1),
            Operand::Register(Register::R2),
        );
        assert_eq!(format!("{}", inst), "addx r0, r1, r2");
    }

    // ──────────────────────── Constant ────────────────────────

    #[test]
    fn test_constant_from_str() {
        let c: Constant = "hello".into();
        match &c {
            Constant::String(s) => assert_eq!(s.as_ref(), "hello"),
        }
    }

    #[test]
    fn test_constant_from_string() {
        let c: Constant = "world".to_string().into();
        match &c {
            Constant::String(s) => assert_eq!(s.as_ref(), "world"),
        }
    }

    #[test]
    fn test_constant_display() {
        let c: Constant = "hello".into();
        assert_eq!(format!("{}", c), "\"hello\"");
    }

    // ──────────────────────── FunctionId ────────────────────────

    #[test]
    fn test_function_id() {
        let fid = FunctionId::new(42);
        assert_eq!(fid.as_usize(), 42);
        assert_eq!(fid.as_isize(), 42);
        assert_eq!(format!("{}", fid), "42");
    }

    // ──────────────────────── Module ────────────────────────

    #[test]
    fn test_module_new() {
        let module = Module::new(
            Some("test".to_string()),
            vec![],
            HashMap::new(),
            vec![],
        );
        assert_eq!(module.name, Some("test".to_string()));
        assert!(module.constants.is_empty());
        assert!(module.symtab.is_empty());
        assert!(module.instructions.is_empty());
    }

    #[test]
    fn test_module_display() {
        let module = Module::new(
            Some("main".to_string()),
            vec![Constant::from("hello")],
            HashMap::new(),
            vec![Bytecode::empty(Opcode::Halt)],
        );
        let display = format!("{}", module);
        assert!(display.contains("Module main"));
        assert!(display.contains("\"hello\""));
        assert!(display.contains("halt"));
    }
}
