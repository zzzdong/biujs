function add(a, b) {
  return a + b;
}
function outer(x) {
  return function inner(y) {
    return x + y;
  };
}
add(1, 2) + outer(3)(4);
