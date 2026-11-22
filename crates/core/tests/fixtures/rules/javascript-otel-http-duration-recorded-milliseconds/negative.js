import { metrics } from '@opentelemetry/api';
const meter = metrics.getMeter('service');
const duration = meter.createHistogram('http.server.request.duration', { unit: 's' });
const started = performance.now();
duration.record((performance.now() - started) / 1000);
const other = meter.createHistogram('other.duration', { unit: 's' });
other.record(performance.now() - started);
const legacy = meter.createHistogram('http.client.request.duration', { unit: 'ms' });
legacy.record(performance.now() - started);
