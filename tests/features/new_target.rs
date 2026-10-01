//! Feature tests for `new.target` (ES 14.2.3 / 12.3.5.1).
//!
//! `new.target` is per frame: the constructor for a `[[Construct]]` frame,
//! `undefined` for an ordinary call, inherited by arrows from the frame that
//! created them, and propagated through `super()`.

use crate::helpers::{eval_bool, eval_string};

#[test]
fn new_target_is_undefined_in_a_plain_call() {
    assert_eq!(
        eval_string("function f() { return String(new.target); } f()"),
        "undefined"
    );
    assert_eq!(
        eval_string("var o = { m: function () { return String(new.target); } }; o.m()"),
        "undefined"
    );
    // `Function.prototype.call` is still a plain call.
    assert_eq!(
        eval_string("function f() { return String(new.target); } f.call(null)"),
        "undefined"
    );
}

#[test]
fn new_target_is_the_constructor_under_new() {
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { seen = new.target; }
             var f = new F();
             seen === F && f instanceof F"
        ),
        true
    );
    // `new F` without parentheses behaves the same.
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { seen = new.target; }
             new F;
             seen === F"
        ),
        true
    );
}

#[test]
fn new_target_within_class_constructor() {
    assert_eq!(
        eval_bool(
            "var seen;
             class C { constructor() { seen = new.target; } }
             new C();
             seen === C"
        ),
        true
    );
}

#[test]
fn derived_class_sees_the_class_being_constructed() {
    // Every constructor in the chain observes the outermost `new` target.
    assert_eq!(
        eval_string(
            "var baseNt, parentNt, childNt;
             class Base { constructor() { baseNt = new.target.name; } }
             class Parent extends Base { constructor() { parentNt = new.target.name; super(); } }
             class Child extends Parent { constructor() { childNt = new.target.name; super(); } }
             new Child();
             childNt + ',' + parentNt + ',' + baseNt"
        ),
        "Child,Child,Child"
    );
}

#[test]
fn arrow_function_inherits_new_target() {
    // An arrow has no [[Construct]]: it sees the enclosing frame's new.target.
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { var arrow = () => new.target; seen = arrow(); }
             new F();
             seen === F"
        ),
        true
    );
    assert_eq!(
        eval_string(
            "function F() { var arrow = () => new.target; return String(arrow()); }
             F()"
        ),
        "undefined"
    );
}

#[test]
fn new_target_in_class_method_is_undefined() {
    // A method is not a constructor frame, even when reached through an instance.
    assert_eq!(
        eval_string("class C { m() { return String(new.target); } } new C().m()"),
        "undefined"
    );
}

/// P1 的验收：`[[Call]]` 与 `[[Construct]]` 现在共用**一份**帧驱动
/// （`VM::drive_bytecode_frame` + `FrameMode`）。四种模式差异以前是靠两份 100 行的
/// 拷贝各自维持的，所以这里把它们一次钉住 —— 任何一处被合一改坏都会红。
#[test]
fn call_and_construct_frames_share_one_driver() {
    // 差异一：构造帧的 `this` 是新建的实例（调用帧的是传进来的）。
    assert_eq!(
        eval_bool(
            "function C() { this.tag = 'instance'; }
             C.prototype.probe = function () { return this.tag; };
             new C().probe() === 'instance'"
        ),
        true
    );
    // 差异二：构造器返回对象就用它，返回非对象仍用 `this`。
    assert_eq!(
        eval_bool(
            "function Obj() { return { replaced: true }; }
             function Prim() { this.kept = true; return 42; }
             new Obj().replaced === true && new Prim().kept === true"
        ),
        true
    );
    // 差异三：`function_val` / `new.target` —— `Reflect.construct` 的第三个参数。
    assert_eq!(
        eval_bool(
            "function Base() { this.nt = new.target; }
             var Other = function () {};
             var d = Reflect.construct(Base, []);
             var o = Reflect.construct(Base, [], Other);
             d.nt === Base && o.nt === Other"
        ),
        true
    );
    // 差异四：`super()` 把 `new.target` 传上去（构造帧 → 构造帧）。
    assert_eq!(
        eval_bool(
            "class A { constructor() { this.nt = new.target; } }
             class B extends A {}
             new B().nt === B"
        ),
        true
    );
    // 合一之后"唯一入口"也不该丢东西：代理当构造器（B45 的路径，走 construct 陷阱）。
    assert_eq!(
        eval_bool(
            "function C() { this.tag = 1; }
             var P = new Proxy(C, {});
             new P().tag === 1"
        ),
        true
    );
}
