# Embedded type's declaration arrives in a new file after the caller was indexed.
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'base_methods.go': 'package app\n\nfunc (b *Base) Start() {}\n',
 'other.go': 'package app\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'base.go': 'package app\n\ntype Base struct{}\n'},
 {'base.go': None},
]
