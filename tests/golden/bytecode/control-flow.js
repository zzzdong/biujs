var total = 0;
for (var i = 0; i < 3; i = i + 1) {
  if (i === 1) {
    continue;
  } else {
    total = total + i;
  }
}
while (total > 2) {
  total = total - 1;
}
total;
