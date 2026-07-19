package main

import "os"

func run(paths []string) {
	for _, p := range paths {
		func() {
			f, err := os.Open(p)
			if err != nil {
				return
			}
			defer f.Close()
			process(f)
		}()
	}
}
