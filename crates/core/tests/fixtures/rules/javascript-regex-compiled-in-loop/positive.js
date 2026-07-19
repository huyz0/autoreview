function check(items) {
  for (const item of items) {
    const re = new RegExp("^" + item);
    console.log(re.test(item));
  }
}
