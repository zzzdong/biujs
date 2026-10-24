var { a, b: renamed, ...rest } = { a: 1, b: 2, c: 3, d: 4 };
var [first, , third = 9, ...tail] = [1, 2, 3, 4, 5];
a + renamed + rest.c + first + third + tail.length;
