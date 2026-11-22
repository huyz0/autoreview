package storage

import filesystem "os"

func Publish(staging, destination string) {
    _ = filesystem.Rename(staging, destination)
}
