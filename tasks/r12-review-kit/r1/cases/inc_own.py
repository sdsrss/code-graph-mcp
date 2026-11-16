# A typed call bound to a promoted method; then T gains its own method in another file.
FILES = {
 'go.mod': 'module example.com/app\n',
 'base.go': 'package app\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n',
 'server.go': 'package app\n\ntype Server struct{ Base }\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
}
EDITS = [
 {'server2.go': 'package app\n\nfunc (s *Server) Start() {}\n'},
 {'server2.go': None},
]
