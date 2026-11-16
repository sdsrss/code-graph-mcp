# a.Server embeds b.Base (other package); b also has a Server with Start. Caller in another file of a.
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\nimport "example.com/app/b"\n\ntype Server struct{ b.Base }\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'a/run2.go': 'package a\n\nfunc (s *Server) Run2() { s.Stop() }\n',
 'b/base.go': 'package b\n\ntype Base struct{}\n\nfunc (b *Base) Start() {}\nfunc (b *Base) Stop() {}\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
}
