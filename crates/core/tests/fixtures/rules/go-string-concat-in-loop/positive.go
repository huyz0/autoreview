package main

func f(items []string) string {
	s := ""
	for i := 0; i < len(items); i++ {
		s = s + items[i]
	}
	return s
}
