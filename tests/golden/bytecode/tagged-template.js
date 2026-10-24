function tag(strings, value) {
  return strings[0] + value + strings[1];
}
var name = "world";
tag`hello ${name}!`;
