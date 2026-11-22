package storage

import filesystem "os"

func publish(staging, destination string) error {
    return filesystem.Rename(staging, destination)
}

type mover interface {
    Rename(string, string)
}

func moveWith(filesystem mover, staging, destination string) {
    filesystem.Rename(staging, destination)
}
