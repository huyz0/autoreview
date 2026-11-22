import { metrics } from '@opentelemetry/api';
const meter = metrics.getMeter('service');
const duration = meter.createHistogram('http.server.request.duration', { unit: 's' });
const started = performance.now();
duration.record(performance.now() - started, { route: '/' });
