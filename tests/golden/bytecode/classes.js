class Base {
  constructor(x) {
    this.x = x;
  }
  value() {
    return this.x;
  }
}
class Derived extends Base {
  constructor(x) {
    super(x);
  }
  value() {
    return super.value() + 1;
  }
}
new Derived(2).value();
