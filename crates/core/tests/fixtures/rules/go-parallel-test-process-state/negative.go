package review

import "testing"

func TestSequentialConfig(t *testing.T) {
	t.Setenv("CONFIG_PATH", "fixture.toml")
}

func TestReadOnly(t *testing.T) {
	t.Parallel()
	t.Log("read-only test")
}

func TestOtherReceiver(t *testing.T) {
	t.Parallel()
	var other environment
	other.Setenv("CONFIG_PATH", "fixture.toml")
}

type environment struct{}

func (environment) Setenv(key, value string) {}
