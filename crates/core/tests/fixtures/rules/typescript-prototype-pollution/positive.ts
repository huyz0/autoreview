function merge(target: any, value: any) {
  target.__proto__ = value;
}
