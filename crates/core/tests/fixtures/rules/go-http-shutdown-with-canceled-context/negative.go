package example

import (
	"context"
	"net/http"
	"time"
)

func drainGracefully(signal context.Context, srv *http.Server) error {
	<-signal.Done()
	drain, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return srv.Shutdown(drain)
}

func stopImmediately(srv *http.Server) error {
	return srv.Close()
}
