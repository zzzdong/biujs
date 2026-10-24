function* gen(n) {
  yield n;
  yield n + 1;
  return n + 2;
}
var it = gen(1);
it.next();
it.next();
