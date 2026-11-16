# embedded type's declaration arrives in a later run
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'other/other.go': 'package other\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n'},
]
