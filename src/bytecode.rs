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
    /// Declared name and parameter count of every bytecode function, keyed by
    /// function id. Function objects expose these as `name` / `length`.
    pub func_info: HashMap<u32, (String, usize)>,
    /// Ids of the functions declared as `function*`. A call to one of these
    /// creates a generator object instead of pushing a frame.
    pub generators: std::collections::HashSet<u32>,
    /// Ids of the functions declared `async`. A call to one of these runs the
    /// body (which may `await`) and answers a promise settled with its outcome.
    pub asyncs: std::collections::HashSet<u32>,
    /// Ids of constructors declared in a class with an `extends` clause. Their
    /// `this` is uninitialized until `super()` runs.
    pub derived_ctors: std::collections::HashSet<u32>,
    /// The pc of every function's trailing `Ret`.
    ///
    /// A suspended generator that is resumed with a *return* completion has to
    /// re-enter its body at a `Ret` so that `Opcode::Ret`'s `finally` dispatch
    /// runs (and so that the finally's `ResumeExc` epilogue finds a `Ret` to
    /// fall through to). The suspension point is a `Yield`, which has no `Ret`
    /// of its own, so the function's exit is looked up here instead.
    ///
    /// `default_instructions()` is the fallback when a function has no `Ret`
    /// at all (a module built by hand, as the unit tests do).
    pub exit_pc: HashMap<u32, usize>,
    pub instructions: Vec<Instr>,
    pub debug_instructions: BTreeMap<usize, crate::compiler::ir::Instruction>,
}

/// 一个函数的字节码体（深改第 2 步的第一步）：它的**指令范围**。
///
/// 现在它是 `Module::symtab` 的**派生视图**，不存新状态 —— 因此不可能与那张表不一致。
/// 下一步把 EH 元数据（try/catch/finally 的 pc 区间）填进来时，它才变成真正的新数据；
/// 那时改的是这里，而不是再散到 VM 的各条 arm 里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionBody {
    /// 入口 pc（来自 `Module::symtab`）。
    pub start: usize,
    /// 下一个函数的入口（或程序末尾）。**注意它不等于"尾 `Ret` 的后一位"**：
    /// catch / finally 块发射在尾 `Ret` 之后，所以范围必须覆盖它们。
    pub end: usize,
}

/// 一个 try 区域（对应 ChakraCore 的 EH 表）。
///
/// **由编译期发射的 `Try` 指令派生**，不是另一张手写表：`Try` 的 catch/finally 是
/// 相对偏移，配上它自己的 pc 就是绝对地址；区域边界由配对的 `EndTry` 定（嵌套时用
/// 深度计数）。所以用 [`Module::eh_regions_of`] 扫一遍指令流就能得到它，
/// 不可能与"实际发射了什么"不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EhRegion {
    /// `Try` 指令自己的 pc。
    pub start: usize,
    /// 受保护范围的**结束**（不含）：配对的 `EndTry` 的 pc。
    pub end: usize,
    /// catch 处理器的 pc（没有 catch 块时为 `None`）。
    pub catch: Option<usize>,
    /// finally 块的 pc（没有 finally 块时为 `None`）。
    pub finally: Option<usize>,
}

impl Module {
    pub fn new(
        name: impl Into<Option<String>>,
        constants: Vec<Constant>,
        symtab: HashMap<FunctionId, usize>,
        func_info: HashMap<u32, (String, usize)>,
        generators: std::collections::HashSet<u32>,
        asyncs: std::collections::HashSet<u32>,
        derived_ctors: std::collections::HashSet<u32>,
        exit_pc: HashMap<u32, usize>,
        instructions: Vec<Instr>,
    ) -> Self {
        Self {
            name: name.into(),
            constants,
            symtab,
            func_info,
            generators,
            asyncs,
            derived_ctors,
            exit_pc,
            instructions,
            debug_instructions: BTreeMap::new(),
        }
    }

    /// 某个函数的字节码体：它的指令范围（`symtab` 的入口 + `exit_pc` 的尾 `Ret`）。
    ///
    /// **派生视图，不存新状态**，所以不可能与那两张表不一致。手搭的 `Module`
    /// （单测里常见）可能既没有 `Ret` 也没有入口，那时返回 `None` —— 调用方各自兜底，
    /// 见 `VM::drive_bytecode_frame` 里哨兵返回地址的处理。
    pub fn body_of(&self, func_id: u32) -> Option<FunctionBody> {
        let start = *self.symtab.get(&FunctionId::new(func_id))?;
        // 结束 = 下一个函数的起点（函数按入口 pc 顺序排在指令流里），最后一个则到程序末尾。
        //
        // **不能用 `exit_pc + 1`**：catch / finally 块是发射在函数尾 `Ret` **之后**的，
        // 那样算出来的范围会把处理器切在函数外面（实测抓到过：handler pc 51 而 end 51）。
        let end = self
            .symtab
            .values()
            .copied()
            .filter(|pc| *pc > start)
            .min()
            .unwrap_or(self.instructions.len());
        Some(FunctionBody { start, end })
    }

    /// 某个函数的 EH 区域表：从它自己发射的 `Try` / `EndTry` **派生**。
    ///
    /// 嵌套的 try 用深度计数配对（`EnterTry` 加一、`EndTry` 减一，回到 0 即本区域的
    /// 结束）。catch/finally 的 0 偏移表示"没有这一块"（`Try` 指令的约定）。
    pub fn eh_regions_of(&self, func_id: u32) -> Vec<EhRegion> {
        let Some(body) = self.body_of(func_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut open: Vec<EhRegion> = Vec::new();
        for pc in body.start..body.end.min(self.instructions.len()) {
            match &self.instructions[pc] {
                Instr::Try {
                    catch_offset,
                    finally_offset,
                } => {
                    let catch = catch_offset.as_immd();
                    let finally = finally_offset.as_immd();
                    open.push(EhRegion {
                        start: pc,
                        // 先写函数边界；真正结束时用配对的 `EndTry` 修正。
                        end: body.end,
                        catch: (catch != 0).then_some((pc as isize + catch) as usize),
                        finally: (finally != 0).then_some((pc as isize + finally) as usize),
                    });
                }
                Instr::EndTry {} => {
                    if let Some(mut region) = open.pop() {
                        region.end = pc;
                        out.push(region);
                    }
                }
                _ => {}
            }
        }
        // 没配对的 `Try`（掉出函数边界）：仍按函数边界收尾，宁可多报也不漏。
        out.extend(open);
        out
    }

    /// 所有函数的体，按入口 pc 排序。给"逐条过一遍"的工具与测试用。
    pub fn bodies(&self) -> Vec<(u32, FunctionBody)> {
        let mut out: Vec<(u32, FunctionBody)> = self
            .func_info
            .keys()
            .chain(self.generators.iter())
            .chain(self.asyncs.iter())
            .copied()
            .collect::<std::collections::BTreeSet<u32>>()
            .into_iter()
            .filter_map(|id| self.body_of(id).map(|body| (id, body)))
            .collect();
        out.sort_by_key(|(_, body)| body.start);
        out
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

/// 指令的**唯一定义处**。
///
/// 一行 = 一个 opcode + 它按位置排列的操作数（按语义命名）；表按 **arity 分成四组**，
/// 于是"这条指令有几个操作数"在定义处就看得见，而不是靠变体上方的一行注释。
///
/// 由这张表生成 `Instr` 本身与 `opcode()` / `arity()` / `arity_of()` / `slots()` / `from_parts()`。
/// P0b 会把读写集与跳转/调用性质（`desc()`）也挂在这里。**新增一条指令 = 加一行。**
///
/// 表里的 arity 是**权威值**：取发射端（`compiler/codegen.rs`）与执行端
/// （`vm/mod.rs` 的 `run_instruction`）实际使用的槽位数的较大者。逐条核对的结果是
/// **没有一条指令的操作数超过 3 个**，而 `Try` / `Halt` / `Ret` / `EndTry` / `ResumeExc` /
/// `PrologueEnd` 在旧编码里都带着无意义的填充槽（`Try` 甚至得用 `triple` 才装得下两个偏移，
/// 第三个槽永远是 0）。
///
/// 表里还可以给一行加一个**可选的 kind 标记**（`@Jump` / `@Call` / …），未标即 `Normal`。
/// 标记由本宏生成 [`Instr::kind`]，且**没有兜底分支** —— 往表里加一条指令而忘了想它属于
/// 哪一类，编译期就会报错。
/// 一条指令在执行期的**控制流效应**。这是给"扫字节码的人"用的（回填校验、
/// 代码生成后处理、将来的 CFG 层），**不是**热路径：主循环仍然直接解构命名字段。
///
/// 与"操作数角色"（P0b 的读写集）是两件事：`Kind` 说这条指令**会做什么**，
/// 读写集说它**碰哪些操作数**。`Try` 有跳转目标却是 `Normal`（它登记处理点后照样往下走），
/// `IterNext` 写一个布尔寄存器也是 `Normal`（分支由随后的 `BrIf` 完成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 顺序执行到 `pc + 1`。
    Normal,
    /// 可能把 `pc` 设到别处：`Jump` / `BrIf`（两者都是**相对**偏移，
    /// 走 `state.jump_offset`）以及 `DelayedJump` / `ResumeExc`（**绝对** pc，
    /// 走 `state.jump`）。
    Jump,
    /// **指令本身就是一次调用**：从栈上取 `argc` 个参数并进入被调方
    /// （`Call` / `CallEx` / `CallNative` / `CallMethod` / `CallSpread` /
    /// `CallSuperSpread`）或构造（`New` / `NewSpread`）。
    ///
    /// 这**不是**"可能触发用户代码"：`PropGet` 会跑 getter、`InstanceOf` 会跑
    /// `Symbol.hasInstance`、`ToString` 会跑 `valueOf`，它们都仍算 `Normal` ——
    /// `Kind` 描述的是指令**自身的形状**，不是"它能跑多少 JS"。
    Call,
    /// 结束当前帧：`Ret` / `Halt`。
    Return,
    /// 抛出异常：`ThrowExc`。
    Throw,
    /// 挂起当前帧、把控制权交回 resumer：`Yield` / `Await`。
    Suspend,
    /// 栈指针簿记（`rsp` / `rbp` 的编译期维护），不参与 JS 值流：
    /// `PushC` / `PopC` / `MovC` / `AddC` / `SubC`。
    Bookkeeping,
}

/// 表里的 `@Jump` 这类标记 → `Kind::Jump`；没标 → `Kind::Normal`。
///
/// 宏里要写 `make_kind!($( $k )?)`，那个可选重复展开成"什么都没有"或一个 ident，
/// 于是这里正好用两条规则接住。
macro_rules! make_kind {
    () => {
        Kind::Normal
    };
    ($k:ident) => {
        Kind::$k
    };
}

/// 一个操作数在指令里的**角色**。由表里字段后面的标记生成（`Mov { dst: w, src: r }`）。
///
/// 与 [`Kind`] 正交：`Kind` 说这条指令**会做什么**，`Role` 说它**碰哪些操作数**。
/// "跳转偏移当寄存器读"那类 bug 出在这一层，所以这里的每个字段都必须表态 ——
/// 表里漏写角色键不入（宏的匹配器要求 `字段: 角色`），于是它是编译期错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 读：值流的入口。
    Read,
    /// 写：值流的出口。
    Write,
    /// 先读后写（in/out）：`New.callee` / `CallMethod.callee` 这类 —— 读出来当 `this`，
    /// 再把解析结果写回同一个位置。
    ReadWrite,
    /// **相对**偏移：`pc = pc + off`（`Jump` / `BrIf` / `Try`）。
    RelPc,
    /// **绝对** pc：`pc = off`（`DelayedJump` / `ResumeExc`）。
    AbsPc,
    /// 元数据：常量下标、`argc`、SEH 深度、名字符号、编译期栈指针……不参与值流。
    Meta,
}

/// 表里的角色字母 → [`Role`]。
macro_rules! role_of {
    (r) => {
        Role::Read
    };
    (w) => {
        Role::Write
    };
    (rw) => {
        Role::ReadWrite
    };
    (jr) => {
        Role::RelPc
    };
    (ja) => {
        Role::AbsPc
    };
    (n) => {
        Role::Meta
    };
}

/// 一个**相对**偏移：`pc = pc + off`（`Jump` / `BrIf` / `Try`）。
///
/// 与 [`AbsPc`] 分成两个类型，是为了让"混用"**写不出来** —— `Jump` 是相对、
/// `DelayedJump` 是绝对，这个差异以前只存在于实现里（§3.2.1 第 1 条）。
/// 两个类型都由表里的角色标记生成（`jr` / `ja`），没有第二个需要人肉同步的地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelPc(Operand);

impl RelPc {
    /// 从已有操作数构造（`from_parts` 这类通用路径用）。
    pub fn new(op: Operand) -> Self {
        Self(op)
    }

    /// 立即数形式。
    pub fn immediate(offset: isize) -> Self {
        Self(Operand::Immd(offset))
    }

    /// 偏移值。VM 读它、回填也算它。
    pub fn as_immd(&self) -> isize {
        self.0.as_immd()
    }

    /// 回填：改写偏移。
    pub fn set(&mut self, offset: isize) {
        self.0 = Operand::Immd(offset);
    }

    /// 当普通操作数看待（`slots()` / `Display` 需要统一的视图）。
    pub fn into_operand(self) -> Operand {
        self.0
    }
}

/// 一个**绝对** pc：`pc = off`（`DelayedJump` / `ResumeExc` 收尾用）。
/// 见 [`RelPc`] 说明为什么要分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsPc(Operand);

impl AbsPc {
    /// 从已有操作数构造。
    pub fn new(op: Operand) -> Self {
        Self(op)
    }

    /// 立即数形式。
    pub fn immediate(pc: isize) -> Self {
        Self(Operand::Immd(pc))
    }

    /// pc 值。
    pub fn as_immd(&self) -> isize {
        self.0.as_immd()
    }

    /// 回填：改写 pc。
    pub fn set(&mut self, pc: isize) {
        self.0 = Operand::Immd(pc);
    }

    /// 当普通操作数看待。
    pub fn into_operand(self) -> Operand {
        self.0
    }
}

/// 表里的角色 → **字段类型**。
///
/// 只对 pc 角色成立：`jr`/`ja` 能唯一确定类型，于是类型由角色派生、不需要手写第二个
/// match。其余角色（`r`/`w`/`rw`/`n`）**收不掉** —— 读也可以是立即数/常量
/// （`Mov rv, true`），那些是 regalloc 的决定，字节码不该替它决定（§3.2 的陷阱）。
macro_rules! field_ty {
    (jr) => {
        RelPc
    };
    (ja) => {
        AbsPc
    };
    ($_other:ident) => {
        Operand
    };
}

/// `Operand` → 字段类型（`from_parts` 的通用路径）。
macro_rules! into_field {
    (jr, $e:expr) => {
        RelPc::new($e)
    };
    (ja, $e:expr) => {
        AbsPc::new($e)
    };
    ($_t:ident, $e:expr) => {
        $e
    };
}

/// 字段 → `Operand`（`slots()` 的统一视图）。
macro_rules! field_operand {
    (jr, $e:expr) => {
        $e.into_operand()
    };
    (ja, $e:expr) => {
        $e.into_operand()
    };
    ($_t:ident, $e:expr) => {
        $e
    };
}

/// 一条指令的操作数一览（[`Instr::desc`] 的返回值）。
///
/// 只服务**工具与测试**：回填校验、dump、以及与实现的互校。解释器主循环仍然直接
/// 解构命名字段，所以这里可以放心用 `Vec`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desc {
    /// `(操作数, 角色)`，按表里的字段顺序。
    pub fields: Vec<(Operand, Role)>,
    /// 控制流类别（与 [`Instr::kind`] 一致）。
    pub kind: Kind,
}

impl Desc {
    /// 参与值流入口的操作数（`Read` 与 `ReadWrite`）。
    pub fn reads(&self) -> impl Iterator<Item = Operand> + '_ {
        self.fields
            .iter()
            .filter(|(_, r)| matches!(r, Role::Read | Role::ReadWrite))
            .map(|(op, _)| *op)
    }

    /// 参与值流出口的操作数（`Write` 与 `ReadWrite`）。
    pub fn writes(&self) -> impl Iterator<Item = Operand> + '_ {
        self.fields
            .iter()
            .filter(|(_, r)| matches!(r, Role::Write | Role::ReadWrite))
            .map(|(op, _)| *op)
    }

    /// 会被当作 pc 用的操作数，附带"相对还是绝对"。
    pub fn targets(&self) -> impl Iterator<Item = (Operand, Role)> + '_ {
        self.fields
            .iter()
            .filter(|(_, r)| matches!(r, Role::RelPc | Role::AbsPc))
            .copied()
    }
}

macro_rules! define_instrs {
    (
        $( $z:ident {} $( @$zk:ident )? ),* ;
        $( $o1:ident { $f1:ident : $f1r:ident } $( @$k1:ident )? ),* ;
        $( $o2:ident { $g1:ident : $g1r:ident, $g2:ident : $g2r:ident } $( @$k2:ident )? ),* ;
        $( $o3:ident {
            $h1:ident : $h1r:ident,
            $h2:ident : $h2r:ident,
            $h3:ident : $h3r:ident
        } $( @$k3:ident )? ),*
    ) => {
        #[derive(Debug, Clone, Copy)]
        pub enum Instr {
            $( $z {}, )*
            $( $o1 { $f1: field_ty!($f1r) }, )*
            $( $o2 { $g1: field_ty!($g1r), $g2: field_ty!($g2r) }, )*
            $( $o3 {
                $h1: field_ty!($h1r),
                $h2: field_ty!($h2r),
                $h3: field_ty!($h3r),
            }, )*
        }

        impl Instr {
            /// 这条指令是什么（诊断，以及既有按 opcode 分支的代码）。
            pub fn opcode(&self) -> Opcode {
                match self {
                    $( Instr::$z {} => Opcode::$z, )*
                    $( Instr::$o1 { .. } => Opcode::$o1, )*
                    $( Instr::$o2 { .. } => Opcode::$o2, )*
                    $( Instr::$o3 { .. } => Opcode::$o3, )*
                }
            }

            /// 本变体声明的操作数个数 —— 由表决定，不再是"永远 3 个"。
            pub fn arity(&self) -> usize {
                match self {
                    $( Instr::$z {} => 0, )*
                    $( Instr::$o1 { .. } => 1, )*
                    $( Instr::$o2 { .. } => 2, )*
                    $( Instr::$o3 { .. } => 3, )*
                }
            }

            /// 某个 opcode 声明的操作数个数（`Instr` 还没有实例时用）。
            pub fn arity_of(op: Opcode) -> usize {
                match op {
                    $( Opcode::$z => 0, )*
                    $( Opcode::$o1 => 1, )*
                    $( Opcode::$o2 => 2, )*
                    $( Opcode::$o3 => 3, )*
                }
            }

            /// 这条指令在执行期的**控制流效应**（表里 `@Kind` 标记；未标 = `Normal`）。
            ///
            /// 注意定义：`Kind` 说的是"执行时会发生什么"，**不是**"有没有 pc 字段"。
            /// 所以 `Try`（登记异常处理点、然后顺序往下走）是 `Normal`，
            /// `IterNext`（把布尔写进寄存器、由随后的 `BrIf` 分支）也是 `Normal`。
            pub fn kind(&self) -> Kind {
                match self {
                    $( Instr::$z {} => make_kind!($( $zk )?), )*
                    $( Instr::$o1 { .. } => make_kind!($( $k1 )?), )*
                    $( Instr::$o2 { .. } => make_kind!($( $k2 )?), )*
                    $( Instr::$o3 { .. } => make_kind!($( $k3 )?), )*
                }
            }

            /// 同上，但只有 opcode 时用（例如回填/扫描时手上没有实例）。
            pub fn kind_of(op: Opcode) -> Kind {
                match op {
                    $( Opcode::$z => make_kind!($( $zk )?), )*
                    $( Opcode::$o1 => make_kind!($( $k1 )?), )*
                    $( Opcode::$o2 => make_kind!($( $k2 )?), )*
                    $( Opcode::$o3 => make_kind!($( $k3 )?), )*
                }
            }

            /// 这条指令的操作数角色（按表里的字段顺序）。
            pub fn roles_of(op: Opcode) -> Vec<Role> {
                match op {
                    $( Opcode::$z => vec![], )*
                    $( Opcode::$o1 => vec![ role_of!($f1r) ], )*
                    $( Opcode::$o2 => vec![ role_of!($g1r), role_of!($g2r) ], )*
                    $( Opcode::$o3 => vec![
                        role_of!($h1r),
                        role_of!($h2r),
                        role_of!($h3r),
                    ], )*
                }
            }

            /// 同上，但带上字段名 —— 互校测试要拿名字回源码里取证（见
            /// `bytecode::tests::roles_match_the_vm_implementation`）。
            pub fn field_roles(op: Opcode) -> Vec<(&'static str, Role)> {
                match op {
                    $( Opcode::$z => vec![], )*
                    $( Opcode::$o1 => vec![ (stringify!($f1), role_of!($f1r)) ], )*
                    $( Opcode::$o2 => vec![
                        (stringify!($g1), role_of!($g1r)),
                        (stringify!($g2), role_of!($g2r)),
                    ], )*
                    $( Opcode::$o3 => vec![
                        (stringify!($h1), role_of!($h1r)),
                        (stringify!($h2), role_of!($h2r)),
                        (stringify!($h3), role_of!($h3r)),
                    ], )*
                }
            }

            /// 操作数一览 + 控制流类别。工具与测试用（见 [`Desc`]）。
            pub fn desc(&self) -> Desc {
                let slots = self.slots();
                Desc {
                    fields: slots[..self.arity()]
                        .iter()
                        .copied()
                        .zip(Self::roles_of(self.opcode()))
                        .collect(),
                    kind: self.kind(),
                }
            }

            /// 表里**所有** opcode，顺序即表顺序。
            ///
            /// 给"逐条过一遍"的工具与测试用：`kind()` 没有兜底分支，所以往表里加指令
            /// 必须表态；这个常量则保证测试能把每一条都走一遍（否则漏测是无声的）。
            pub const ALL: &'static [Opcode] = &[
                $( Opcode::$z, )*
                $( Opcode::$o1, )*
                $( Opcode::$o2, )*
                $( Opcode::$o3, )*
            ];

            /// 按 opcode 组装。`Instruction::UnaryOp` / `BinaryOp` 携带**运行时 opcode**，
            /// 所以必须有这条路径；调用方请用 [`Self::unary`] / [`Self::binary`]，它们核对 arity。
            /// （旧编码下这两种写法完全不做检查：任何 opcode 都能配上任何数量的槽位。）
            pub fn from_parts(op: Opcode, a: Operand, b: Operand, c: Operand) -> Self {
                match op {
                    $( Opcode::$z => Instr::$z {}, )*
                    $( Opcode::$o1 => Instr::$o1 { $f1: into_field!($f1r, a) }, )*
                    $( Opcode::$o2 => Instr::$o2 {
                        $g1: into_field!($g1r, a),
                        $g2: into_field!($g2r, b),
                    }, )*
                    $( Opcode::$o3 => Instr::$o3 {
                        $h1: into_field!($h1r, a),
                        $h2: into_field!($h2r, b),
                        $h3: into_field!($h3r, c),
                    }, )*
                }
            }

            pub fn unary(op: Opcode, dst: Operand, src: Operand) -> Self {
                assert_eq!(
                    Self::arity_of(op), 2,
                    "{op:?} is not a unary (2-operand) instruction"
                );
                Self::from_parts(op, dst, src, Operand::Immd(0))
            }

            pub fn binary(op: Opcode, dst: Operand, lhs: Operand, rhs: Operand) -> Self {
                assert_eq!(
                    Self::arity_of(op), 3,
                    "{op:?} is not a binary (3-operand) instruction"
                );
                Self::from_parts(op, dst, lhs, rhs)
            }

            /// 操作数按位置拷出，未用到的位置补 `Operand::Immd(0)`。
            ///
            /// 给需要**泛型地**看待一条指令的消费者用：目前只有 `Display for Instr`
            /// 与断言填充规则的单测。解释器主循环**不再**用它 —— `run_instruction`
            /// 现在按命名字段解构（P0a-2），所以这次拷贝不在热路径上。
            ///
            /// （P0a 的原始计划是"P0a-2 做完就删掉它"。实际做下来 `Display` 仍需要
            /// 一个统一的视图，否则要为 93 个变体各写一遍格式串 —— 那正是这张表要
            /// 消灭的重复知识。于是保留，但把它的用途限定在工具侧。）
            pub fn slots(&self) -> [Operand; 4] {
                match self {
                    $( Instr::$z {} => [Operand::Immd(0); 4], )*
                    $( Instr::$o1 { $f1 } => Instr::pack(&[field_operand!($f1r, *$f1)]), )*
                    $( Instr::$o2 { $g1, $g2 } => Instr::pack(&[
                        field_operand!($g1r, *$g1),
                        field_operand!($g2r, *$g2),
                    ]), )*
                    $( Instr::$o3 { $h1, $h2, $h3 } => Instr::pack(&[
                        field_operand!($h1r, *$h1),
                        field_operand!($h2r, *$h2),
                        field_operand!($h3r, *$h3),
                    ]), )*
                }
            }

            /// `slots()` 的填充规则：不足 4 个的位置补 `Operand::Immd(0)`。
            #[inline]
            fn pack(used: &[Operand]) -> [Operand; 4] {
                let mut slots = [Operand::Immd(0); 4];
                slots[..used.len()].copy_from_slice(used);
                slots
            }
        }
    };
}

define_instrs! {
// ── arity 0 ──
        Halt {} @Return,
        Ret {} @Return,
        EndTry {},
        ResumeExc {} @Jump,
        PrologueEnd {} @Suspend
    ;
// ── arity 1 ──
        DeclareLexical { name: n },
        Push { src: r },
        Pop { dst: w },
        PushC { src: n } @Bookkeeping,
        PopC { dst: n } @Bookkeeping,
        Jump { offset: jr } @Jump,
        DelegateOpen { iter: r },
        DelegateClose { iter: n },
        MakeArray { dst: w },
        MarkHole { array: r },
        MakeObject { dst: w },
        ThrowExc { value: r } @Throw,
        LoadException { dst: w },
        LoadThis { dst: w },
        LoadNewTarget { dst: w },
        LoadCurrentFunction { dst: w },
        Arguments { dst: w },
        IterClose { iter: r },
        RequireObjectCoercible { src: r }
    ;
// ── arity 2 ──
        LoadConst { dst: w, index: n },
        InitLexical { name: n, value: r },
        SetFunctionName { func: r, name: r },
        LoadEnv { dst: w, name: n },
        MovC { dst: n, src: n } @Bookkeeping,
        Call { func: n, argc: n } @Call,
        CallEx { callee: r, argc: n } @Call,
        CallNative { callee: r, argc: n } @Call,
        Mov { dst: w, src: r },
        Not { dst: w, src: r },
        BitNot { dst: w, src: r },
        Neg { dst: w, src: r },
        TypeOf { dst: w, src: r },
        TypeOfEnv { dst: w, name: n },
        MakeIter { dst: w, src: r },
        ArrayPush { array: r, value: r },
        ArrayPushSpread { array: r, src: r },
        StoreEnv { name: n, value: r },
        Try { catch_offset: jr, finally_offset: jr },
        DelayedJump { target: ja, seh_depth: n } @Jump,
        CreateClosure { dst: w, func: n },
        New { callee: rw, argc: n } @Call,
        MakeFuncObj { dst: w, func: n },
        ClosureVar { name: n, value: r },
        Yield { dst: w, value: r } @Suspend,
        Await { dst: w, src: r } @Suspend,
        ToString { dst: w, src: r },
        ToNumber { dst: w, src: r },
        MakeRest { dst: w, from: n }
    ;
// ── arity 3 ──
        MakeRegExp { dst: w, source: n, flags: n },
        AddC { dst: n, src: n, value: n } @Bookkeeping,
        SubC { dst: n, src: n, value: n } @Bookkeeping,
        BrIf { condition: r, true_target: jr, false_target: jr } @Jump,
        BitAnd { dst: w, lhs: r, rhs: r },
        BitOr { dst: w, lhs: r, rhs: r },
        BitXor { dst: w, lhs: r, rhs: r },
        Shl { dst: w, lhs: r, rhs: r },
        Shr { dst: w, lhs: r, rhs: r },
        UShr { dst: w, lhs: r, rhs: r },
        Addx { dst: w, lhs: r, rhs: r },
        Subx { dst: w, lhs: r, rhs: r },
        Mulx { dst: w, lhs: r, rhs: r },
        Divx { dst: w, lhs: r, rhs: r },
        Remx { dst: w, lhs: r, rhs: r },
        Pow { dst: w, lhs: r, rhs: r },
        And { dst: w, lhs: r, rhs: r },
        Or { dst: w, lhs: r, rhs: r },
        Less { dst: w, lhs: r, rhs: r },
        LessEqual { dst: w, lhs: r, rhs: r },
        Greater { dst: w, lhs: r, rhs: r },
        GreaterEqual { dst: w, lhs: r, rhs: r },
        Equal { dst: w, lhs: r, rhs: r },
        NotEqual { dst: w, lhs: r, rhs: r },
        StrictEqual { dst: w, lhs: r, rhs: r },
        StrictNotEqual { dst: w, lhs: r, rhs: r },
        InstanceOf { dst: w, lhs: r, rhs: r },
        In { dst: w, lhs: r, rhs: r },
        IterNext { dst: w, has_next: w, src: r },
        IndexGet { dst: w, object: r, index: r },
        IndexSet { object: r, index: r, value: r },
        PropGet { dst: w, object: r, property: r },
        PropSet { object: r, property: r, value: r },
        PropDelete { dst: w, object: r, property: r },
        IndexDelete { dst: w, object: r, index: r },
        CallMethod { callee: rw, property: r, argc: n } @Call,
        MakeArrowFuncObj { dst: w, func: n, captured_this: r },
        CallSpread { callee: r, this: r, args: r } @Call,
        CallSuperSpread { callee: r, this: r, args: r } @Call,
        NewSpread { dst: w, ctor: r, args: r } @Call
}

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.opcode())?;
        // 只打印本变体真正声明的操作数：旧实现固定打印三个槽位，其中可能是填充值。
        let slots = self.slots();
        if let Some((last, rest)) = slots[..self.arity()].split_last() {
            for slot in rest {
                write!(f, " {slot},")?;
            }
            write!(f, " {last}")?;
        }
        Ok(())
    }
}

// `PartialEq`/`Eq` 是 P0a 加的：表与测试都要能比较"这条指令是什么"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    /// load_const dst, const_id
    LoadConst,
    /// declare_lexical name — register a script-scope `let`/`const`/`class`
    /// name in the script's declarative record, uninitialized (its dead zone).
    DeclareLexical,
    /// init_lexical name, value — initialize a script-scope lexical binding
    /// (the declaration; the only write the dead zone allows)
    InitLexical,
    /// set_function_name func, name — give a just-created function/class object
    /// its name (NamedEvaluation)
    SetFunctionName,
    /// make_regexp dst, source, flags — build a RegExp object from a literal
    MakeRegExp,
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
    /// bitand dst, src1, src2 (bitwise AND &)
    BitAnd,
    /// bitor dst, src1, src2 (bitwise OR |)
    BitOr,
    /// bitxor dst, src1, src2 (bitwise XOR ^)
    BitXor,
    /// shl dst, src1, src2 (left shift <<)
    Shl,
    /// shr dst, src1, src2 (sign-propagating right shift >>)
    Shr,
    /// ushr dst, src1, src2 (unsigned right shift >>>)
    UShr,
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
    /// pow dst, src1, src2 (exponentiation `**`)
    Pow,
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
    /// typeof_env dst, name — `typeof x` for an unresolvable-at-compile-time
    /// name: yields "undefined" instead of throwing ReferenceError.
    TypeOfEnv,
    /// instanceof dst, src1, src2
    InstanceOf,
    /// in dst, src1, src2
    In,
    /// make_iter dst, src
    /// Remember an iterator opened by `yield*`: an abrupt completion of the
    /// enclosing generator has to close it (ES 14.4.14).
    DelegateOpen,
    /// The `yield*` finished normally; its iterator is no longer pending.
    DelegateClose,
    MakeIter,
    /// iter_next dst, has_next, src
    IterNext,
    /// make_array dst
    MakeArray,
    /// array_push dst, src
    /// mark_hole array — turn the element just appended into a hole (`[1,,2]`)
    MarkHole,
    ArrayPush,
    /// array_push_spread dst, src — append every element of `src` to `array`
    ArrayPushSpread,
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
    /// prop_delete dst, obj, prop
    PropDelete,
    /// index_delete dst, obj, idx
    IndexDelete,
    /// store_env name, value — assign into the global environment
    StoreEnv,
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
    /// load_new_target dst — `new.target` of the current frame (undefined for a
    /// plain call)
    LoadNewTarget,
    /// load_current_function dst — the function object whose body is running
    /// (used by `super` to find the [[HomeObject]] recorded at class definition)
    LoadCurrentFunction,
    /// make_func_obj dst, func_id
    MakeFuncObj,
    /// make_arrow_func_obj dst, func_id, captured_this
    MakeArrowFuncObj,
    /// closure_var name, value — push a captured variable for the next MakeArrowFuncObj
    ClosureVar,
    /// arguments dst — materialise the current frame's `arguments` object
    Arguments,
    /// iter_close iter — signal early exit to a protocol iterator
    IterClose,
    /// require_object_coercible src — ES 7.2.1, raises a TypeError for
    /// `undefined` / `null`. Emitted as the first step of an object
    /// destructuring pattern, which must reject a nullish source even when the
    /// pattern is empty (`{} = null` reads no property, so there would
    /// otherwise be nothing to notice).
    RequireObjectCoercible,
    /// Suspend the enclosing generator: record the yielded value, hand the
    /// current frame to [`crate::vm::VM::generator_resume`] and stop the
    /// nested execution loop (the VM jumps past the last instruction, which is
    /// what terminates a nested `step` loop).
    Yield,
    /// prologue_end — end of a generator's parameter prologue. A generator
    /// function binds its parameters when it is *called*; the VM parks the
    /// frame here until the first `next()` continues past it. Emitted only for
    /// generators, so ordinary functions never carry it.
    PrologueEnd,
    /// await dst, src — suspend an `async` function on `src` until it settles.
    /// The engine has no event loop, so the VM drains the microtask queue on the
    /// spot until the awaited promise is no longer pending, then writes its
    /// fulfilment value to `dst` (or throws its reason).
    Await,
    /// to_string dst, src — ES ToString (objects via ToPrimitive("string"))
    ToString,
    /// to_number dst, src — ES ToNumber (objects via ToPrimitive("number"))
    ToNumber,
    /// make_rest dst, from_index — collect arguments[from_index..] into an array
    MakeRest,
    /// call_spread result, callee, this, args — call with args taken from an array
    CallSpread,
    /// call_super_spread callee, this, args — `super(...)`: like CallSpread but
    /// the callee frame inherits the caller's `new.target`
    CallSuperSpread,
    /// new_spread dst, ctor, args — construct with args taken from an array
    NewSpread,
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
            Opcode::MakeRegExp => write!(f, "make_regexp"),
            Opcode::DeclareLexical => write!(f, "declare_lexical"),
            Opcode::SetFunctionName => write!(f, "set_function_name"),
            Opcode::InitLexical => write!(f, "init_lexical"),
            Opcode::LoadEnv => write!(f, "load_env"),
            Opcode::Mov => write!(f, "mov"),
            Opcode::Not => write!(f, "not"),
            Opcode::BitNot => write!(f, "bitnot"),
            Opcode::BitAnd => write!(f, "bitand"),
            Opcode::BitOr => write!(f, "bitor"),
            Opcode::BitXor => write!(f, "bitxor"),
            Opcode::Shl => write!(f, "shl"),
            Opcode::Shr => write!(f, "shr"),
            Opcode::UShr => write!(f, "ushr"),
            Opcode::Neg => write!(f, "neg"),
            Opcode::Addx => write!(f, "addx"),
            Opcode::Subx => write!(f, "subx"),
            Opcode::Mulx => write!(f, "mulx"),
            Opcode::Divx => write!(f, "divx"),
            Opcode::Remx => write!(f, "remx"),
            Opcode::Pow => write!(f, "pow"),
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
            Opcode::TypeOfEnv => write!(f, "typeof_env"),
            Opcode::InstanceOf => write!(f, "instanceof"),
            Opcode::In => write!(f, "in"),
            Opcode::DelegateOpen => write!(f, "delegate_open"),
            Opcode::DelegateClose => write!(f, "delegate_close"),
            Opcode::MakeIter => write!(f, "make_iter"),
            Opcode::IterNext => write!(f, "iter_next"),
            Opcode::MakeArray => write!(f, "make_array"),
            Opcode::MarkHole => write!(f, "mark_hole"),
            Opcode::ArrayPush => write!(f, "array_push"),
            Opcode::ArrayPushSpread => write!(f, "array_push_spread"),
            Opcode::MakeObject => write!(f, "make_object"),
            Opcode::IndexGet => write!(f, "index_get"),
            Opcode::IndexSet => write!(f, "index_set"),
            Opcode::PropGet => write!(f, "prop_get"),
            Opcode::PropSet => write!(f, "prop_set"),
            Opcode::PropDelete => write!(f, "prop_delete"),
            Opcode::IndexDelete => write!(f, "index_delete"),
            Opcode::StoreEnv => write!(f, "store_env"),
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
            Opcode::LoadNewTarget => write!(f, "load_new_target"),
            Opcode::LoadCurrentFunction => write!(f, "load_current_function"),
            Opcode::MakeFuncObj => write!(f, "make_func_obj"),
            Opcode::MakeArrowFuncObj => write!(f, "make_arrow_func_obj"),
            Opcode::ClosureVar => write!(f, "closure_var"),
            Opcode::Arguments => write!(f, "arguments"),
            Opcode::IterClose => write!(f, "iter_close"),
            Opcode::RequireObjectCoercible => write!(f, "require_object_coercible"),
            Opcode::Yield => write!(f, "yield"),
            Opcode::PrologueEnd => write!(f, "prologue_end"),
            Opcode::Await => write!(f, "await"),
            Opcode::ToString => write!(f, "to_string"),
            Opcode::ToNumber => write!(f, "to_number"),
            Opcode::MakeRest => write!(f, "make_rest"),
            Opcode::CallSpread => write!(f, "call_spread"),
            Opcode::CallSuperSpread => write!(f, "call_super_spread"),
            Opcode::NewSpread => write!(f, "new_spread"),
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
        assert_eq!(
            format!("{}", Operand::new_primitive(Primitive::Null)),
            "null"
        );
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
        let inst = Instr::Halt {};
        assert_eq!(inst.opcode(), Opcode::Halt);
        assert_eq!(inst.arity(), 0);
        // 没有操作数就是没有：旧编码在这里是三个填充槽 `Immd(0)`。
        assert!(inst.slots().iter().all(|op| matches!(op, Operand::Immd(0))));
    }

    #[test]
    fn test_bytecode_single() {
        let inst = Instr::Push {
            src: Operand::Register(Register::R0),
        };
        assert_eq!(inst.opcode(), Opcode::Push);
        assert_eq!(inst.arity(), 1);
        assert!(matches!(inst.slots()[0], Operand::Register(Register::R0)));
        // `slots()` 只对"未声明的位置"补 `Immd(0)`。
        assert!(inst.slots()[1..].iter().all(|op| matches!(op, Operand::Immd(0))));
    }

    #[test]
    fn test_bytecode_double() {
        let inst = Instr::Mov {
            dst: Operand::Register(Register::R0),
            src: Operand::Register(Register::R1),
        };
        assert_eq!(inst.opcode(), Opcode::Mov);
        assert_eq!(inst.arity(), 2);
        assert!(matches!(inst.slots()[0], Operand::Register(Register::R0)));
        assert!(matches!(inst.slots()[1], Operand::Register(Register::R1)));
    }

    #[test]
    fn test_bytecode_triple() {
        let inst = Instr::Addx {
            dst: Operand::Register(Register::R0),
            lhs: Operand::Register(Register::R1),
            rhs: Operand::Register(Register::R2),
        };
        assert_eq!(inst.opcode(), Opcode::Addx);
        assert_eq!(inst.arity(), 3);
        assert!(matches!(inst.slots()[0], Operand::Register(Register::R0)));
        assert!(matches!(inst.slots()[1], Operand::Register(Register::R1)));
        assert!(matches!(inst.slots()[2], Operand::Register(Register::R2)));
    }

    /// `Try` 曾经必须用 `triple` 才装得下两个偏移，第三个槽永远是 0；
    /// 现在它有且只有两个操作数。这是"固定三槽"消失后最直接的一个证据。
    #[test]
    fn test_try_has_no_padding_slot() {
        let inst = Instr::Try {
            catch_offset: RelPc::immediate(11),
            finally_offset: RelPc::immediate(22),
        };
        assert_eq!(inst.arity(), 2);
        assert_eq!(Instr::arity_of(Opcode::Try), 2);
        assert_eq!(format!("{inst}"), "try 11, 22");
    }

    /// 表是 arity 的唯一来源：`unary` / `binary` 只接受操作数个数对得上的 opcode。
    #[test]
    fn test_dynamic_opcode_constructors_check_arity() {
        let dst = Operand::Register(Register::R0);
        let src = Operand::Register(Register::R1);
        assert!(matches!(
            Instr::unary(Opcode::Not, dst, src),
            Instr::Not { .. }
        ));
        assert!(matches!(
            Instr::binary(Opcode::Addx, dst, src, src),
            Instr::Addx { .. }
        ));
        assert_eq!(Instr::arity_of(Opcode::Addx), 3);
        assert_eq!(Instr::arity_of(Opcode::Not), 2);
    }

    #[test]
    #[should_panic(expected = "is not a binary")]
    fn test_binary_rejects_a_two_operand_opcode() {
        let op = Operand::Register(Register::R0);
        Instr::binary(Opcode::Mov, op, op, op);
    }

    /// Display 只打印本变体声明的操作数：旧实现固定打印三个槽位。
    #[test]
    fn test_display_omits_padding() {
        assert_eq!(format!("{}", Instr::Halt {}), "halt");
        assert_eq!(
            format!(
                "{}",
                Instr::Push {
                    src: Operand::Register(Register::R0)
                }
            ),
            "push r0"
        );
    }

    #[test]
    fn test_bytecode_display() {
        let inst = Instr::Addx { dst: Operand::Register(Register::R0), lhs: Operand::Register(Register::R1), rhs: Operand::Register(Register::R2) };
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
            HashMap::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
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
            HashMap::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            HashMap::new(),
            vec![Instr::Halt {}],
        );
        let display = format!("{}", module);
        assert!(display.contains("Module main"));
        assert!(display.contains("\"hello\""));
        assert!(display.contains("halt"));
    }

    // ──────────────────────── Kind（P0b 的控制流分类）────────────────────────

    fn count_of(kind: Kind) -> usize {
        Instr::ALL.iter().filter(|op| Instr::kind_of(**op) == kind).count()
    }

    /// 计数断言：改表时这几个数会变，逼你确认"是有意改的"。
    ///
    /// 它同时也是"标记没被手滑删掉"的守卫 —— 删掉一个 `@Call`，这里立刻红。
    #[test]
    fn kind_markers_cover_the_table() {
        assert_eq!(Instr::ALL.len(), 93, "表里的指令条数");
        assert_eq!(count_of(Kind::Call), 8, "Call* / New*");
        assert_eq!(count_of(Kind::Jump), 4, "Jump / BrIf / DelayedJump / ResumeExc");
        assert_eq!(count_of(Kind::Return), 2, "Ret / Halt");
        assert_eq!(count_of(Kind::Throw), 1, "ThrowExc");
        assert_eq!(count_of(Kind::Suspend), 3, "Yield / Await / PrologueEnd");
        assert_eq!(count_of(Kind::Bookkeeping), 5, "PushC / PopC / MovC / AddC / SubC");
        // 其余全是 Normal；没有兜底分支的 kind() 保证"没标"就是 Normal。
        let marked = count_of(Kind::Call)
            + count_of(Kind::Jump)
            + count_of(Kind::Return)
            + count_of(Kind::Throw)
            + count_of(Kind::Suspend)
            + count_of(Kind::Bookkeeping);
        assert_eq!(count_of(Kind::Normal), Instr::ALL.len() - marked);
    }

    /// 每个函数的字节码体都得是自洽的一段（深改第 2 步的守卫）。
    ///
    /// 这三条断言看着显而易见，但它们是后面把 EH 元数据挂进 `FunctionBody` 的前提：
    /// 在那之前必须先确认"一个函数的代码范围"这个概念本身是可靠的。
    #[test]
    fn function_bodies_are_well_formed() {
        let mut compiler = crate::compiler::Compiler::new();
        let module = compiler
            .compile(
                "function plain(a) { return a + 1; }
                 function* gen() { yield 1; }
                 async function asy() { return 2; }
                 function outer() { var x = 1; return function inner() { return x; }; }
                 class C { constructor() { this.v = 1; } m() { return this.v; } }
                 function withTry() { try { return 1; } finally { return 2; } }
                 plain(1); withTry();",
            )
            .expect("compilation failed");

        let bodies = module.bodies();
        assert!(!bodies.is_empty(), "至少要解析出几个函数");

        for (id, body) in &bodies {
            assert!(body.start < body.end, "{id} 的范围是空的: {body:?}");
            // 最后一条必须是 `Ret`：函数的出口由它定义（`exit_pc` 就是它的 pc）。
            let last = module.instructions[body.end - 1];
            assert_eq!(
                last.kind(),
                Kind::Return,
                "{id} 的出口不是 Ret，而是 {last}"
            );
            assert!(body.end <= module.instructions.len(), "{id} 的范围越过了程序末尾");
            // 尾 `Ret` 必须在范围内 —— 而范围**还要覆盖它之后的 catch/finally**：
            // 早先用 `exit_pc + 1` 当 end，正好把处理器切在了函数外面（实测抓到过）。
            if let Some(exit) = module.exit_pc.get(id) {
                assert!(
                    (body.start..body.end).contains(exit),
                    "{id} 的尾 Ret pc {exit} 不在范围 {body:?} 内"
                );
            }
        }
        // 带 finally 的那个函数：范围必须比它的尾 `Ret` 更大（处理器在后面）。
        let with_try = module
            .bodies()
            .into_iter()
            .find(|(id, _)| module.eh_regions_of(*id).iter().any(|r| r.finally.is_some()));
        let (id, body) = with_try.expect("至少要有一个带 finally 的函数");
        let exit = module.exit_pc[&id];
        assert!(
            body.end >= exit + 1,
            "{id} 的范围 {body:?} 连尾 Ret({exit}) 都没包住"
        );

        // 所有函数都顺序排在指令流里，因此范围两两不重叠。
        for pair in bodies.windows(2) {
            assert!(
                pair[0].1.end <= pair[1].1.start,
                "{} 的范围 {:?} 与 {} 的 {:?} 重叠了 —— 函数边界的假设不成立",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
    }

    /// EH 区域表是从指令流**派生**的，所以有一条强不变量可验：
    /// **区域数 == 实际发射的 `Try` 条数**（不漏也不多），且每条区域自洽、catch/finally
    /// 落在函数自己的范围内。这比"手写一张表然后祈祷它对"可靠得多。
    #[test]
    fn eh_regions_are_derived_from_the_emitted_try_instructions() {
        let mut compiler = crate::compiler::Compiler::new();
        let module = compiler
            .compile(
                "function a() { try { 1 } catch (e) { 2 } }
                 function b() { try { 3 } finally { 4 } }
                 function c() { try { 5 } catch (e) { 6 } finally { 7 } }
                 function d() { try { try { 8 } catch (e) { 9 } } finally { 10 } }
                 a(); b(); c(); d();",
            )
            .expect("compilation failed");

        let emitted_tries = module
            .instructions
            .iter()
            .filter(|i| matches!(i, Instr::Try { .. }))
            .count();
        assert!(emitted_tries >= 5, "至少要 5 个 try（含一个嵌套），实际 {emitted_tries}");

        let mut regions = Vec::new();
        for (id, body) in module.bodies() {
            for region in module.eh_regions_of(id) {
                assert!(region.start < region.end, "{id} 的区域是空的: {region:?}");
                assert!(
                    region.end <= body.end,
                    "{id} 的区域越过了函数边界: {region:?} vs {body:?}"
                );
                for pc in region.catch.into_iter().chain(region.finally) {
                    assert!(
                        (body.start..body.end).contains(&pc),
                        "{id} 的处理器 pc {pc} 不在函数范围内 {body:?}"
                    );
                }
                regions.push(region);
            }
        }
        assert_eq!(
            regions.len(),
            emitted_tries,
            "派生的区域数必须等于实际发射的 Try 条数 —— 差一个就是推导漏了或多了"
        );

        // 逐个函数核对表格的**语义**（迁移运行时之前先把"表该长什么样"钉死）：
        // 名字 → id 走 `func_info`。
        let id_of = |name: &str| -> u32 {
            *module
                .func_info
                .iter()
                .find(|(_, (n, _))| n == name)
                .unwrap_or_else(|| panic!("没有函数 {name}"))
                .0
        };
        let one = |name: &str| -> EhRegion {
            let mut rs = module.eh_regions_of(id_of(name));
            assert_eq!(rs.len(), 1, "{name} 应该只有一个 try，实际 {:?}", rs);
            rs.pop().unwrap()
        };
        // try/catch
        let r = one("a");
        assert!(r.catch.is_some() && r.finally.is_none(), "a: {r:?}");
        // try/finally：**catch 与 finally 指向同一个块** —— 没有 catch 时，finally 兼作
        // handler（先进 finally，再 rethrow）。这是发射端的事实，不是表的推断。
        let r = one("b");
        assert!(
            r.catch.is_some() && r.catch == r.finally,
            "b（try/finally）的 catch 应与 finally 同址: {r:?}"
        );
        // try/catch/finally：两者都有，且**不同址**
        let r = one("c");
        assert!(
            r.catch.is_some() && r.finally.is_some() && r.catch != r.finally,
            "c（try/catch/finally）的 catch 与 finally 应不同址: {r:?}"
        );
        // 嵌套：外层 finally、内层 catch，内层被外层包含
        let mut nested = module.eh_regions_of(id_of("d"));
        assert_eq!(nested.len(), 2, "d 应该有嵌套的两个 try，实际 {:?}", nested);
        nested.sort_by_key(|r| r.start);
        let (outer, inner) = (nested[0], nested[1]);
        // 外层是 try/finally（catch 与 finally 同址，同上），内层是 try/catch。
        assert!(
            outer.catch.is_some() && outer.catch == outer.finally,
            "d 的外层是 try/finally，catch 应与 finally 同址: {outer:?}"
        );
        assert!(
            inner.catch.is_some() && inner.finally.is_none(),
            "d 的内层: {inner:?}"
        );
        assert!(
            outer.start < inner.start && inner.end <= outer.end,
            "d 的内层没被外层包含: {outer:?} / {inner:?}"
        );

        regions.sort_by_key(|r| r.start);
        for pair in regions.windows(2) {
            if pair[1].start < pair[0].end {
                // 后一个在前一个里面：要么它被包含，要么两者起点相同（不可能）
                assert!(
                    pair[1].end <= pair[0].end,
                    "区域没有正确嵌套: {:?} 与 {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    /// 表的顺序与 `ALL` 一致（`ALL` 是拿来做工具/测试的，顺序即表顺序）。
    #[test]
    fn opcode_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for op in Instr::ALL {
            assert!(seen.insert(format!("{op:?}")), "{op:?} 在表里出现了两次");
        }
    }

    /// 从 `run_instruction` 的源码里切出每条 arm：(opcode 名字列表, arm 全文)。
    ///
    /// arm 头一定在**基线缩进**（12 空格）且以 `Instr::` 开头；or-模式的续行是
    /// 14 空格 + `| Instr::`，不会被误当成新 arm。
    fn run_instruction_arms(source: &str) -> Vec<(Vec<String>, String)> {
        let start = source.find("fn run_instruction").expect("VM 里没有 run_instruction");
        let rest = &source[start..];
        let end = rest[1..].find("\n    fn ").map(|i| i + 1).unwrap_or(rest.len());
        let body = &rest[..end];
        let m = body.find("match *inst {").expect("run_instruction 不再 match *inst");
        let text = &body[m..];

        let mut heads = Vec::new();
        let mut cursor = 0;
        for line in text.split_inclusive('\n') {
            // 正好 12 个空格 + `Instr::` = arm 头；or-模式的续行是 14 空格 + `| `。
            if line.starts_with("            Instr::") {
                heads.push(cursor);
            }
            cursor += line.len();
        }
        let mut arms = Vec::new();
        for (i, &h) in heads.iter().enumerate() {
            let slice_end = heads.get(i + 1).copied().unwrap_or(text.len());
            let arm = &text[h..slice_end];
            let head = &arm[..arm.find("=>").expect("arm 没有 =>")];
            let mut ops = Vec::new();
            let mut idx = 0;
            while let Some(p) = head[idx..].find("Instr::") {
                let s = idx + p + "Instr::".len();
                let name: String = head[s..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                ops.push(name);
                idx = s;
            }
            arms.push((ops, arm.to_string()));
        }
        arms
    }

    /// **元测试**：kind 标记说的是"这条指令执行时会发生什么"，而唯一能证明它没说谎的
    /// 东西就是 `run_instruction` 的实现本身。所以这里直接读 VM 的源码取证 ——
    /// 判据不是又一份手写知识，而是实现。
    ///
    /// 注意这些断言是**单向**的（"标了 Call 就必须真的在调用"）：反过来不成立，
    /// 因为 `PropGet` 会跑 getter、`InstanceOf` 会跑 `Symbol.hasInstance` ——
    /// 它们能跑 JS，但自身形状不是"一次调用"，仍算 `Normal`。
    #[test]
    fn kind_matches_the_vm_implementation() {
        let source = include_str!("vm/mod.rs");
        let arms = run_instruction_arms(source);

        // 覆盖性：表里的每一条都要能在这段源码里找到（也就顺带验证了上面的切分器
        // 没有漏掉多行模式）。
        let mut covered = std::collections::HashSet::new();
        for (ops, _) in &arms {
            for op in ops {
                covered.insert(op.clone());
            }
        }
        for op in Instr::ALL {
            let name = format!("{op:?}");
            assert!(covered.contains(&name), "{name} 在 run_instruction 里没有 arm");
        }

        let rules: &[(Kind, &[&str])] = &[
            (
                Kind::Call,
                &[
                    // P1-2c 起，调用类 opcode 的证据主要是它把活交给统一入口；
                    // 其余几条是还没改道的路径（`CallNative` / `CallSpread` 等）。
                    "self.enter_call(",
                    "self.invoke(",
                    "self.construct(",
                    "self.invoke_with_new_target(",
                    "builtins::call_native(",
                ],
            ),
            (Kind::Jump, &["self.state.jump(", "self.state.jump_offset("]),
            (Kind::Suspend, &["generator_yielded", "await_value"]),
            (Kind::Throw, &["self.handle_throw("]),
            (Kind::Bookkeeping, &["rsp"]),
            // `Ret` / `Halt` 由 `step()` 处理，这里的 arm 只可能是 `unreachable!`。
            (Kind::Return, &["unreachable!("]),
        ];

        for (ops, arm) in &arms {
            for name in ops {
                let op = Instr::ALL
                    .iter()
                    .copied()
                    .find(|o| format!("{o:?}") == *name)
                    .unwrap_or_else(|| panic!("表里没有 {name}"));
                let kind = Instr::kind_of(op);
                let Some((_, evidence)) = rules.iter().find(|(k, _)| *k == kind) else {
                    continue; // Normal：不做断言（见上面的说明）
                };
                assert!(
                    evidence.iter().any(|e| arm.contains(e)),
                    "{name} 标了 {kind:?}，但它的 arm 里找不到 {evidence:?} 中的任何一个；\
                     arm 正文是：\n{arm}"
                );
            }
        }
    }

    /// 从一条 arm 头里解出每个变体绑定了哪些字段：`opcode → [(字段名或别名, 绑定名)]`。
    ///
    /// 位置不在这里定 —— `Instr::Yield { value, .. }` 里的 `value` 是第 1 个字段，
    /// 光看模式看不出来（裸字段名既是字段名也是绑定名），所以位置留给调用方按表解析。
    fn arm_bindings(head: &str) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        for seg in head.split('|') {
            let Some(name_start) = seg.find("Instr::") else {
                continue;
            };
            let name: String = seg[name_start + "Instr::".len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let Some(open) = seg.find('{') else { continue };
            let Some(close) = seg.rfind('}') else { continue };
            let mut binds: Vec<String> = Vec::new();
            for item in seg[open + 1..close].split(',') {
                let item = item.trim();
                if item.is_empty() || item == ".." {
                    continue;
                }
                let bind = match item.split_once(':') {
                    Some((_, b)) => b.trim(),
                    None => item,
                };
                binds.push(bind.to_string());
            }
            out.push((name, binds));
        }
        out
    }

    /// **元测试**：字段角色（`Mov { dst: w, src: r }`）与 `run_instruction` 的实际用法
    /// 必须一致 —— 判据是那个 arm 到底有没有 `get_value` / `set_value` 这个字段。
    ///
    /// 这样一来 `desc()` 里的读写集**不是**第三份人肉维护的知识，而是被实现校验过的派生。
    #[test]
    fn roles_match_the_vm_implementation() {
        let arms = run_instruction_arms(include_str!("vm/mod.rs"));

        // 两处例外：`-1`（"无操作数"）约定让 VM 先 match 字段再取值，于是证据长成
        //     match value { Operand::Immd(-1) => …, src => self.get_value(src)? }
        // 名字对不上，但语义就是"读这个字段"。P0b 的类型收紧会把它换成
        // `Option<Operand>`，届时这条豁免可以删掉。
        let marker_read_arms = ["Yield", "Await"];

        let mut checked = 0;
        for (_, arm) in &arms {
            let head = &arm[..arm.find("=>").expect("arm 没有 =>")];
            for (opcode_name, binds) in arm_bindings(head) {
                let op = Instr::ALL
                    .iter()
                    .copied()
                    .find(|o| format!("{o:?}") == opcode_name)
                    .unwrap_or_else(|| panic!("表里没有 {opcode_name}"));
                let fields = Instr::field_roles(op);
                let names: Vec<&str> = fields.iter().map(|(f, _)| *f).collect();
                for (idx, (field, role)) in fields.iter().enumerate() {
                    // 某个绑定的位置：`a2` 这类别名自带下标；否则按"绑定名 = 字段名"
                    // 在表里的位置找（裸字段名既是字段名也是绑定名）。
                    let bind = binds.iter().find(|b| {
                        match b.strip_prefix('a').filter(|n| {
                            !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
                        }) {
                            Some(n) => n.parse::<usize>().ok() == Some(idx),
                            None => names.get(idx).is_some_and(|f| f == &b.as_str()),
                        }
                    });
                    let Some(bind) = bind else { continue };
                    let bind = bind.clone();
                    let reads = arm.contains(&format!("self.get_value({bind})"))
                        || arm.contains(&format!("self.resolve_property_key({bind}"))
                        || arm.contains(&format!("self.raw_stack_value({bind})"));
                    let writes = arm.contains(&format!("self.set_value({bind},"));
                    let name = format!("{opcode_name}.{field}");
                    let ok = match role {
                        Role::Read => reads || marker_read_arms.contains(&opcode_name.as_str()),
                        Role::Write => writes,
                        Role::ReadWrite => {
                            (reads || marker_read_arms.contains(&opcode_name.as_str())) && writes
                        }
                        // 元数据 / pc：不该出现在值流的读写里。
                        Role::Meta | Role::RelPc | Role::AbsPc => !reads && !writes,
                    };
                    assert!(
                        ok,
                        "{name} 标成 {role:?}，但实现与之不符（读过={reads}、写过={writes}）；\
                         这段 arm 是：\n{arm}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 150, "只核对了 {checked} 个字段，切分器大概坏了");
    }

    /// `desc()` 的口径：字段顺序 = 表顺序，角色 = 表里的标记，`kind` 是同一个。
    #[test]
    fn desc_reports_the_table() {
        let d = Instr::Mov {
            dst: Operand::Register(Register::R0),
            src: Operand::Register(Register::R1),
        }
        .desc();
        assert_eq!(d.kind, Kind::Normal);
        assert_eq!(d.reads().collect::<Vec<_>>(), vec![Operand::Register(Register::R1)]);
        assert_eq!(d.writes().collect::<Vec<_>>(), vec![Operand::Register(Register::R0)]);
        assert_eq!(d.targets().count(), 0);

        let jump = Instr::Jump { offset: RelPc::immediate(7) }.desc();
        assert_eq!(jump.kind, Kind::Jump);
        assert_eq!(jump.reads().count(), 0);
        assert_eq!(
            jump.targets().collect::<Vec<_>>(),
            vec![(Operand::Immd(7), Role::RelPc)],
            "Jump 是相对偏移"
        );

        let call = Instr::Call {
            func: Operand::Immd(3),
            argc: Operand::Immd(2),
        }
        .desc();
        assert_eq!(call.kind, Kind::Call);
        assert_eq!((call.reads().count(), call.writes().count()), (0, 0), "参数在栈上，callee 是函数号");
    }

    /// 类型收紧（P0b-2a）：pc 字段的**类型**由角色派生 —— `jr` → `RelPc`、`ja` → `AbsPc`，
    /// 于是"相对偏移喂给绝对字段"是编译错误（`expected RelPc, found AbsPc`），
    /// 不再是靠人记住 `Jump` 与 `DelayedJump` 的区别。
    ///
    /// 这里是数量断言：相对 5 个（`Jump` 1 + `BrIf` 2 + `Try` 2）、绝对 1 个
    /// （`DelayedJump`）。改表时这个数会变，逼你确认是有意的。
    #[test]
    fn pc_fields_are_typed_by_role() {
        let (mut rel, mut abs) = (0, 0);
        for op in Instr::ALL {
            for (_, role) in Instr::field_roles(*op) {
                match role {
                    Role::RelPc => rel += 1,
                    Role::AbsPc => abs += 1,
                    _ => {}
                }
            }
        }
        assert_eq!((rel, abs), (5, 1), "相对 5（Jump/BrIf×2/Try×2）、绝对 1（DelayedJump）");

        // 两个类型各自的读取/回填写法（也顺便钉住"读不改变调用点"这一点）。
        let mut j = Instr::Jump { offset: RelPc::immediate(3) };
        assert_eq!(match &j { Instr::Jump { offset } => offset.as_immd(), _ => 0 }, 3);
        if let Instr::Jump { offset } = &mut j {
            offset.set(9);
        }
        assert!(format!("{j}").contains('9'), "回填后 Display 应反映新偏移：{j}");
    }

    /// 跳转类指令的偏移**相对还是绝对**：`Jump` / `BrIf` 是相对（`jump_offset`），
    /// `DelayedJump` / `ResumeExc` 是绝对（`jump`）。这条差异以前只存在于实现里，
    /// 现在有两个断言钉住它（P0b 的类型收紧要靠它决定 `RelPc` / `AbsPc`）。
    #[test]
    fn jump_offsets_are_relative_or_absolute() {
        let arms = run_instruction_arms(include_str!("vm/mod.rs"));
        let arm_of = |want: &str| {
            arms.iter()
                .find(|(ops, _)| ops.iter().any(|o| o == want))
                .map(|(_, arm)| arm.clone())
                .unwrap_or_else(|| panic!("找不到 {want} 的 arm"))
        };
        for relative in ["Jump", "BrIf"] {
            let arm = arm_of(relative);
            assert!(arm.contains("jump_offset("), "{relative} 应该是相对跳转");
        }
        for absolute in ["DelayedJump", "ResumeExc"] {
            let arm = arm_of(absolute);
            assert!(arm.contains("self.state.jump("), "{absolute} 应该是绝对跳转");
        }
    }
}
