package fixtures

import "github.com/patrickmn/go-cache"

func NewPending() *cache.Cache {
	return cache.New(cache.NoExpiration, 0)
}
