# valid Go: a.Server gets next() from an embedded interface; package b has its own Server.next
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype nexter interface{ next() }\n\ntype Server struct{ nexter }\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.next() }\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) next() {}\n',
}
