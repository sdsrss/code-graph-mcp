# a call in a package-level func literal is looked up in the previous top-level declaration
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n\ntype Other struct{}\n\nfunc (o *Other) Start() {}\n',
 'a/use.go': '''package a

func (s *Server) Run() {}

var hook = func(s *Other) { s.Start() }
''',
}
