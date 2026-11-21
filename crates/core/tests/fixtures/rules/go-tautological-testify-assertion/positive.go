package check

import (
	"github.com/stretchr/testify/require"
	"testing"
)

func TestValue(t *testing.T) {
	actual := calculate()
	require.Equal(t, actual, actual)
}
