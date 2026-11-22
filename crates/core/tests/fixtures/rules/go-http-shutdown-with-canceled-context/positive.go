package example

import (
	"context"
	"net/http"
)

func Stop(signal context.Context, srv *http.Server) error {
	<-signal.Done()
	return srv.Shutdown(signal)
}
