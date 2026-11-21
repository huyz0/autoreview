console.error("startup failed", process.env.REGION, statusCode);
console.info("checked", Boolean(process.env.SERVICE_API_KEY));
startClient(process.env.SERVICE_API_KEY);
