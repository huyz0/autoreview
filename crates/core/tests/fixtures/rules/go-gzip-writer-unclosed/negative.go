package storage

import (
    zip "compress/gzip"
    "io"
)

func createArchive(dst io.Writer) *zip.Writer {
    return zip.NewWriter(dst)
}

func finishArchive(writer *zip.Writer) {
    writer.Close()
}

func writeArchive(dst io.Writer, data []byte) error {
    writer := createArchive(dst)
    alias := writer
    defer finishArchive(alias)
    _, err := writer.Write(data)
    return err
}
