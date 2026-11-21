package review

import "testing"

func TestConfig(t *testing.T) {
	t.Setenv("CONFIG_PATH", "fixture.toml")
	t.Parallel()
}
