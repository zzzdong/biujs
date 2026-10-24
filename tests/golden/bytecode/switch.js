function classify(n) {
  switch (n) {
    case 0:
      return "zero";
    case 1:
    case 2:
      return "small";
    default:
      return "other";
  }
}
classify(2);
