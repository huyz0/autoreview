package telemetry
import "github.com/prometheus/client_golang/prometheus"
var requestsByRoute = prometheus.NewCounterVec(
    prometheus.CounterOpts{Name: "requests_total"},
    []string{"method", "result"},
)
