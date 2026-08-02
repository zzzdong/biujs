use crate::helpers::eval_js;

#[test]
fn debug_instanceof_step_by_step() {
    // Step 1: Check if Foo.prototype exists
    let js1 = r#"
        function Foo() {}
        typeof Foo.prototype
    "#;
    let result1 = eval_js(js1);
    println!("Step 1 - typeof Foo.prototype: {:?}", result1);

    // Step 2: Create object and check its type
    let js2 = r#"
        function Foo() {}
        var obj = new Foo();
        typeof obj
    "#;
    let result2 = eval_js(js2);
    println!("Step 2 - typeof obj: {:?}", result2);

    // Step 3: Check if we can access obj.constructor
    let js3 = r#"
        function Foo() {}
        var obj = new Foo();
        obj.constructor === Foo
    "#;
    let result3 = eval_js(js3);
    println!("Step 3 - obj.constructor === Foo: {:?}", result3);

    // Step 4: Check instanceof directly
    let js4 = r#"
        function Foo() {}
        var obj = new Foo();
        obj instanceof Foo
    "#;
    let result4 = eval_js(js4);
    println!("Step 4 - obj instanceof Foo: {:?}", result4);
}
