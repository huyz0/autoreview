package storage

import (
    zip "compress/gzip"
    "io"
)

func createArchive(dst io.Writer) *zip.Writer {
    return zip.NewWriter(dst)
}

func writeArchive(dst io.Writer, data []byte) error {
    writer := createArchive(dst)
    _, err := writer.Write(data)
    return err
}
