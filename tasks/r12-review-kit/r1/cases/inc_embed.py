# Embedding added then removed in the type's file; caller elsewhere.
FILES = {
 'go.mod': 'module example.com/app\n',
 'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
 'server.go': 'package app\n\ntype Server struct{}\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
}
EDITS = [
 {'server.go': 'package app\n\ntype Server struct{ Base }\n'},
 {'server.go': 'package app\n\ntype Server struct{}\n'},
]
