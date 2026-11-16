# Two-level embedding: the middle type's embedding changes in its own file.
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{ Mid }\n',
 'mid.go': 'package app\n\ntype Mid struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n',
 'other.go': 'package app\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'mid.go': 'package app\n\ntype Mid struct{ Other }\n'},
 {'mid.go': 'package app\n\ntype Mid struct{}\n'},
 {'mid.go': 'package app\n\ntype Mid struct{ Base }\n'},
]
