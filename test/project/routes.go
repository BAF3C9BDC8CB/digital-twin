package routes

import (
	"fmt"
	"net/http"
)

// Server holds HTTP route mappings.
type Server struct {
	port int
	mux  *http.ServeMux
}

// NewServer creates a new Server instance.
func NewServer(port int) *Server {
	mux := http.NewServeMux()
	srv := &Server{port: port, mux: mux}
	srv.registerRoutes()
	return srv
}

func (s *Server) registerRoutes() {
	s.mux.HandleFunc("GET /health", s.healthHandler)
}

func (s *Server) healthHandler(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, `{"status":"ok"}`)
}
