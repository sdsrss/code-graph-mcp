# a.Server embeds a stdlib type; b.Server (project) has Shutdown. The promoted method is http.Server's.
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\nimport "net/http"\n\ntype Server struct{ *http.Server }\n\nfunc (s *Server) Stop() { s.Shutdown(nil) }\n',
 'a/other.go': 'package a\n\nfunc (s *Server) Stop2() { s.Shutdown(nil) }\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Shutdown(x interface{}) {}\n',
}
