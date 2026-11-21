package example

import "github.com/prometheus/client_golang/prometheus"

func done(active prometheus.Gauge, events prometheus.Counter) {
	active.Add(-1)
	events.Add(1)
}
