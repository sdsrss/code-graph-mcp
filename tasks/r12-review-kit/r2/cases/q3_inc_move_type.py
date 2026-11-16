# the caller's own type's method moves to a nested directory's same-named type
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'a/sub/server.go': 'package sub\n\ntype Server struct{}\n',
}
EDITS = [
 {'a/server.go': 'package a\n\ntype Server struct{}\n', 'a/sub/server.go': 'package sub\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
 {'a/server.go': 'package a\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
]
