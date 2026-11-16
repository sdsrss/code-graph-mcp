# embedded type's declaration arrives later in a file that already had its method
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'base_m.go': 'package app\n\nfunc (b *Base) Start() {}\n',
 'other/other.go': 'package other\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'base.go': 'package app\n\ntype Base struct{}\n'},
]
