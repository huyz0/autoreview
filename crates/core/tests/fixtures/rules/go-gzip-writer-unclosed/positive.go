package storage

import (
    zip "compress/gzip"
    "io"
)

func CreateArchive(dst io.Writer) *zip.Writer {
    return zip.NewWriter(dst)
}

func WriteArchive(dst io.Writer, data []byte) error {
    writer := CreateArchive(dst)
    _, err := writer.Write(data)
    return err
}
