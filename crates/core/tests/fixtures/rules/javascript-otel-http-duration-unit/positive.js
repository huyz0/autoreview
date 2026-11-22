import { metrics } from '@opentelemetry/api';
const meter = metrics.getMeter('service');
const duration = meter.createHistogram('http.server.request.duration', { unit: 'ms', description: 'request duration' });
