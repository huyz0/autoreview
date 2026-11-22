package storage

import (
    "bytes"
    "compress/gzip"
)

func finishReport(compressor *gzip.Writer) {
    err := compressor.Close()
    if err != nil {
        panic(err)
    }
}

func CompressReport(payload []byte) error {
    var buffer bytes.Buffer
    compressor := gzip.NewWriter(&buffer)
    deferred := compressor
    defer finishReport(deferred)
    _, err := compressor.Write(payload)
    return err
}

func WriterAndStatus(buffer *bytes.Buffer) (*gzip.Writer, error) {
    compressor := gzip.NewWriter(buffer)
    return compressor, nil
}
