FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n',
 'a/start_linux.go': '//go:build linux\n\npackage a\n\nfunc (s *Server) start() {}\n',
 'a/start_windows.go': '//go:build windows\n\npackage a\n\nfunc (s *Server) start() {}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.start() }\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) start() {}\n',
}
