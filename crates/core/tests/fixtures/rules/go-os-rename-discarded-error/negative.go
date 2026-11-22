package storage

import filesystem "os"

func MoveStagedFile(staging, destination string) error {
    return filesystem.Rename(staging, destination)
}

type mover interface {
    Rename(string, string)
}

func MoveWith(filesystem mover, staging, destination string) {
    filesystem.Rename(staging, destination)
}
