FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\n\ntype Server struct{ Base }\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
}
