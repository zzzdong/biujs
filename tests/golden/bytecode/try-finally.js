function f() {
  try {
    return 1;
  } finally {
    var side = 2;
  }
}
f();
