import io.micrometer.core.instrument.Timer

fun requestTimer(route: String) = Timer.builder("requests").tag("route", route)
