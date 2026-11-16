# same, but the caller is in the type's own file
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\nimport "example.com/app/b"\n\ntype Server struct{ b.Base }\n\nfunc (s *Server) Run() { s.Start() }\n',
 'b/base.go': 'package b\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
}
