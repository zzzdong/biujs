function counter() {
  let n = 0;
  return function () {
    n = n + 1;
    return n;
  };
}
var c = counter();
c();
c();
