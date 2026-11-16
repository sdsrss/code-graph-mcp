# own method in the caller's directory is deleted, then restored; another package's same-named type keeps it
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n',
 'a/start.go': 'package a\n\nfunc (s *Server) Start() {}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
}
EDITS = [
 {'a/start.go': None},
 {'a/start.go': 'package a\n\nfunc (s *Server) Start() {}\n'},
 {'b/server.go': None},
 {'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
]
