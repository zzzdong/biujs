console.log("Math proto: " + (Object.getPrototypeOf(Math) === Object.prototype));
console.log("Math hasOwnProperty: " + typeof Math.hasOwnProperty);
console.log("Math tag: " + Object.prototype.toString.call(Math));
console.log("JSON proto: " + (Object.getPrototypeOf(JSON) === Object.prototype));
console.log("JSON tag: " + Object.prototype.toString.call(JSON));
console.log("Reflect proto: " + (Object.getPrototypeOf(Reflect) === Object.prototype));
console.log("Reflect tag: " + Object.prototype.toString.call(Reflect));
