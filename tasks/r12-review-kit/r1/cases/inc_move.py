# The receiver's type and its method move between files; a second same-named type appears.
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'x/other.go': 'package x\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'server.go': None, 'server_new.go': 'package app\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
 {'x/server.go': 'package x\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
 {'server_new.go': 'package app\n\ntype Server struct{}\n'},
]
