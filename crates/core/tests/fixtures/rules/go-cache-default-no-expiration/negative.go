package fixtures

import (
	"time"

	"github.com/patrickmn/go-cache"
)

func NewExpiring() *cache.Cache {
	return cache.New(5*time.Minute, time.Minute)
}
