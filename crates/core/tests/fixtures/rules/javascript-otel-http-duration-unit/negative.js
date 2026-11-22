import { metrics } from '@opentelemetry/api';

function httpTimer() {
    const meter = metrics.getMeter('service');
    return standardTimer(meter);
}

const server = metrics.getMeter('server');
const request = server.createHistogram('http.server.request.duration', { unit: 's' });

const storage = metrics.getMeter('storage');
const flush = storage.createHistogram('storage.flush.duration', { unit: 'ms' });
