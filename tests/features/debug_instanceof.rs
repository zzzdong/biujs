use crate::helpers::eval_js;

#[test]
fn debug_instanceof() {
    // Test the simplest instanceof case
    let js = r#"
        function Foo() {}
        var obj = new Foo();
        obj instanceof Foo
    "#;

    let result = eval_js(js);
    println!("Simple instanceof result: {:?}", result);

    // Check if obj.__proto__ equals Foo.prototype
    let js2 = r#"
        function Foo() {}
        var obj = new Foo();
        obj.__proto__ === Foo.prototype
    "#;

    let result2 = eval_js(js2);
    println!("__proto__ check result: {:?}", result2);
}
