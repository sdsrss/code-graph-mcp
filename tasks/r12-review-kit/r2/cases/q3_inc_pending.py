# call buffered while no Start exists; another package's arrives; then the own one arrives in a separate run
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'b/doc.go': 'package b\n',
}
EDITS = [
 {'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
 {'a/start.go': 'package a\n\nfunc (s *Server) Start() {}\n'},
 {'a/start.go': None},
]
