const result = new Promise((resolve, reject) => {
  readValue().then(resolve, reject);
});
const other = new CustomTask(async () => readValue());
const synchronous = new Promise(async => async(1));
