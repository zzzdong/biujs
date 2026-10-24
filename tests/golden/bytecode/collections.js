var array = [1, , 3];
var object = {
  x: 1,
  m() {
    return this.x;
  },
};
array[1] = 2;
object.m() + array.length;
