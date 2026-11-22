package storage

import filesystem "os"

func publish(staging, destination string) {
    _ = filesystem.Rename(staging, destination)
}
