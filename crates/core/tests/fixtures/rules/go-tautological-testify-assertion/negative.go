package check

import (
	"github.com/stretchr/testify/require"
	"testing"
)

func TestIndependentValue(t *testing.T) {
	actual := calculate()
	expected := 42
	require.Equal(t, expected, actual)
	require.Equal(t, calculate(), calculate())
}
