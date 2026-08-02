//! Tests for const variable declaration and reassignment checking.

use crate::helpers::eval_js;

#[test]
fn test_const_basic() {
    // const 应该可以声明变量
    let js = r#"
        const x = 5;
        x
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "const declaration should work");
}

#[test]
fn test_const_no_reassignment() {
    // const 不应该允许重新赋值
    let js = r#"
        const y = 10;
        y = 20;
        y
    "#;
    let result = eval_js(js);
    assert!(result.is_err(), "const reassignment should fail");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("Assignment to constant variable"),
        "Error should mention assignment to constant variable, got: {}",
        err_msg
    );
}

#[test]
fn test_const_no_compound_assignment() {
    // const 不应该允许复合赋值
    let js = r#"
        const y = 10;
        y += 5;
        y
    "#;
    let result = eval_js(js);
    assert!(result.is_err(), "const compound assignment should fail");
}

#[test]
fn test_const_no_increment() {
    // const 不应该允许 ++
    let js = r#"
        const y = 10;
        y++;
        y
    "#;
    let result = eval_js(js);
    assert!(result.is_err(), "const increment should fail");
}

#[test]
fn test_const_no_decrement() {
    // const 不应该允许 --
    let js = r#"
        const y = 10;
        y--;
        y
    "#;
    let result = eval_js(js);
    assert!(result.is_err(), "const decrement should fail");
}

#[test]
fn test_let_reassignment_allowed() {
    // let 应该允许重新赋值
    let js = r#"
        let x = 5;
        x = 10;
        x
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "let reassignment should work");
}

#[test]
fn test_let_compound_assignment_allowed() {
    // let 应该允许复合赋值
    let js = r#"
        let x = 5;
        x += 10;
        x
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "let compound assignment should work");
}

#[test]
fn test_let_increment_allowed() {
    // let 应该允许 ++
    let js = r#"
        let x = 5;
        x++;
        x
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "let increment should work");
}

#[test]
fn test_const_block_scope() {
    // const 应该有块级作用域
    let js = r#"
        const x = 1;
        {
            const x = 2;
        }
        x
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "const block scope should work");
}

#[test]
fn test_const_in_function() {
    // const 在函数中应该工作
    let js = r#"
        function test() {
            const x = 42;
            return x;
        }
        test()
    "#;
    let result = eval_js(js);
    assert!(result.is_ok(), "const in function should work");
}

#[test]
fn test_const_in_function_no_reassignment() {
    // const 在函数中不应该允许重新赋值
    let js = r#"
        function test() {
            const x = 42;
            x = 100;
            return x;
        }
        test()
    "#;
    let result = eval_js(js);
    assert!(
        result.is_err(),
        "const reassignment in function should fail"
    );
}
