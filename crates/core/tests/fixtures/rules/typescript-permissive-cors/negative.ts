import express from "express";
const app = express();
app.use((req: any, res: any, next: any) => {
  res.header("Access-Control-Allow-Origin", "https://example.com");
  next();
});
