use crate::helpers::{eval_number, eval_string};

// ============================================================
// Objects
// ============================================================

/// Object literal with properties
#[test]
fn object_literal_properties() {
    let js = r#"
        let obj = {a: 1, b: 2};
        obj.a + obj.b
    "#;
    assert_eq!(eval_number(js), 3.0);
}

/// Object property set after creation
#[test]
fn object_property_set() {
    let js = r#"
        let obj = {};
        obj.x = 42;
        obj.x
    "#;
    assert_eq!(eval_number(js), 42.0);
}

/// Object property reassignment
#[test]
fn object_property_reassign() {
    let js = r#"
        let obj = {val: 10};
        obj.val = 20;
        obj.val
    "#;
    assert_eq!(eval_number(js), 20.0);
}

/// Object multiple properties read/write
#[test]
fn object_multi_property() {
    let js = r#"
        let obj = {name: "test", count: 5};
        obj.count = obj.count + 1;
        obj.count
    "#;
    assert_eq!(eval_number(js), 6.0);
}

/// Object computed property access (bracket notation)
#[test]
fn object_computed_access() {
    let js = r#"
        let obj = {key: 99};
        let k = "key";
        obj[k]
    "#;
    assert_eq!(eval_number(js), 99.0);
}

/// Object computed property set
#[test]
fn object_computed_set() {
    let js = r#"
        let obj = {};
        let p = "prop";
        obj[p] = 77;
        obj.prop
    "#;
    assert_eq!(eval_number(js), 77.0);
}

/// Object bracket access with string literal (IndexGet via Immd)
#[test]
fn object_literal_bracket_access() {
    let js = r#"
        let obj = {abc: 123};
        obj["abc"]
    "#;
    assert_eq!(eval_number(js), 123.0);
}

/// Object bracket set with string literal (IndexSet via Immd)
#[test]
fn object_literal_bracket_set() {
    let js = r#"
        let obj = {};
        obj["key"] = 456;
        obj.key
    "#;
    assert_eq!(eval_number(js), 456.0);
}

/// Object toString returns [object Object]
#[test]
fn object_to_string_prototype() {
    let js = r#"
        let obj = {};
        obj.toString()
    "#;
    assert_eq!(eval_string(js), "[object Object]");
}

/// Object valueOf returns the object itself
#[test]
fn object_value_of() {
    let js = r#"
        let obj = {a: 1};
        let v = obj.valueOf();
        v.a
    "#;
    assert_eq!(eval_number(js), 1.0);
}

/// typeof on object
#[test]
fn object_typeof() {
    assert_eq!(eval_string("typeof {}"), "object");
}

// ============================================================
// Arrays
// ============================================================

/// Array literal and element access
#[test]
fn array_element_access() {
    assert_eq!(eval_number("[10, 20, 30][1]"), 20.0);
}

/// Array element set via index
#[test]
fn array_element_set() {
    let js = r#"
        let arr = [1, 2, 3];
        arr[1] = 99;
        arr[1]
    "#;
    assert_eq!(eval_number(js), 99.0);
}

/// Array dynamic index access
#[test]
fn array_dynamic_index() {
    let js = r#"
        let arr = [100, 200, 300];
        let i = 2;
        arr[i]
    "#;
    assert_eq!(eval_number(js), 300.0);
}

/// Array bracket access with numeric literal (IndexGet via Primitive)
#[test]
fn array_literal_index() {
    let js = r#"
        let arr = [10, 20, 30];
        arr[1]
    "#;
    assert_eq!(eval_number(js), 20.0);
}

/// Array bracket set with numeric literal (IndexSet via Primitive)
#[test]
fn array_literal_index_set() {
    let js = r#"
        let arr = [1, 2, 3];
        arr[0] = 99;
        arr[0]
    "#;
    assert_eq!(eval_number(js), 99.0);
}

/// Array length is accessible via prototype chain
#[test]
fn array_length() {
    let js = r#"
        let arr = [1, 2, 3, 4, 5];
        arr.length
    "#;
    assert_eq!(eval_number(js), 5.0);
}

/// Array toString via prototype
#[test]
fn array_to_string_prototype() {
    assert_eq!(eval_string("[10, 20, 30].toString()"), "10,20,30");
}

/// Array computed toString call (bracket + string literal)
#[test]
fn array_computed_to_string() {
    assert_eq!(eval_string("[1,2,3][\"toString\"]()"), "1,2,3");
}

/// Array dynamic method name via variable
#[test]
fn array_dynamic_method_name() {
    let js = r#"
        let arr = [5, 10, 15];
        let m = "toString";
        arr[m]()
    "#;
    assert_eq!(eval_string(js), "5,10,15");
}

/// Array valueOf returns the array itself
#[test]
fn array_value_of() {
    let js = r#"
        let arr = [42];
        let v = arr.valueOf();
        v[0]
    "#;
    assert_eq!(eval_number(js), 42.0);
}

/// typeof on array returns "object"
#[test]
fn array_typeof() {
    assert_eq!(eval_string("typeof [1,2,3]"), "object");
}

/// Array of mixed types
#[test]
fn array_mixed_types() {
    let js = r#"
        let arr = [1, "hello", true, null];
        arr[1]
    "#;
    assert_eq!(eval_string(js), "hello");
}

// ============================================================
// Object literal with arrays
// ============================================================

/// Object containing an array
#[test]
fn object_with_array() {
    let js = r#"
        let obj = {items: [1, 2, 3]};
        obj.items.length
    "#;
    assert_eq!(eval_number(js), 3.0);
}

/// Nested property access
#[test]
fn nested_property_access() {
    let js = r#"
        let obj = {inner: {value: 42}};
        obj.inner.value
    "#;
    assert_eq!(eval_number(js), 42.0);
}

// ============================================================
// Prototype chain property lookup
// ============================================================

/// A property added to the prototype should be found via [[Get]]
#[test]
fn prototype_property_inheritance() {
    let js = r#"
        let arr = [1, 2, 3];
        arr.length
    "#;
    assert_eq!(eval_number(js), 3.0);
}
