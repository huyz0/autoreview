package example

import (
	"context"
	"net/http"
	"time"
)

func DrainGracefully(signal context.Context, srv *http.Server) error {
	<-signal.Done()
	drain, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return srv.Shutdown(drain)
}

func StopImmediately(srv *http.Server) error {
	return srv.Close()
}
