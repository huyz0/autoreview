import io.micrometer.core.instrument.Timer

fun requestTimer(requestId: String) = Timer.builder("requests").tag("request.id", requestId)
