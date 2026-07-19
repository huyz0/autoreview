function check(items: string[]) {
  const re = new RegExp("^prefix");
  for (const item of items) {
    console.log(re.test(item));
  }
}
