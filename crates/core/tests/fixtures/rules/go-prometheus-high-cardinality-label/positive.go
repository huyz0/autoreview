package telemetry
import "github.com/prometheus/client_golang/prometheus"
var requests = prometheus.NewCounterVec(
    prometheus.CounterOpts{Name: "requests_total"},
    []string{"method", "request_id"},
)
