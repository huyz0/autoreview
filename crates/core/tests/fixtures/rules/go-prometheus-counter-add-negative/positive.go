package example

import "github.com/prometheus/client_golang/prometheus"

func done(active prometheus.Counter) {
	active.Add(-1)
}
