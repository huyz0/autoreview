import * as stats from 'prom-client';
import * as other from 'test-metrics';
const metric = new stats.Counter({name: 'requests_total', help: 'Requests', labelNames: ['route']});
const custom = new other.Counter({name: 'other', labelNames: ['requestId']});
