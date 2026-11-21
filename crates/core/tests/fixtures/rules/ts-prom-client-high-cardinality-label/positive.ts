import * as stats from 'prom-client';
const metric = new stats.Counter({
  name: 'requests_total',
  help: 'Requests',
  labelNames: ['method', 'requestId'] as const,
});
