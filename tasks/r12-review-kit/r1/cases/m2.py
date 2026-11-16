# The embedded type is renamed in its own file; the struct embedding it and the caller are untouched.
FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n',
 'other.go': 'package app\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
}
EDITS = [
 {'base.go': 'package app\n\ntype Core struct{}\n\nfunc (b *Core) Start() {}\n'},
 {'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n'},
]
