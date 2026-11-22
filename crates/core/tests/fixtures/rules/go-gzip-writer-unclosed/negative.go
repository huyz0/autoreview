package storage

import (
    "bytes"
    "compress/gzip"
)

func finishReport(compressor *gzip.Writer) {
    compressor.Close()
}

func compressReport(payload []byte) {
    var buffer bytes.Buffer
    compressor := gzip.NewWriter(&buffer)
    deferred := compressor
    defer finishReport(deferred)
    compressor.Write(payload)
}
