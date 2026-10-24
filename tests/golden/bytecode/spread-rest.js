function count(...xs) {
  return xs.length;
}
var a = [1, 2];
var b = [0, ...a, 3];
count(...b);
