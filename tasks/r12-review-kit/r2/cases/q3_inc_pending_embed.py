# test fixture of a6b3ca18 extended: a.Server embeds a.Base without Start; b.Server.Start arrives; then a.Base gets Start
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Base struct{}\n\ntype Server struct{ Base }\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.Start() }\n',
 'b/doc.go': 'package b\n',
}
EDITS = [
 {'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n'},
 {'a/base.go': 'package a\n\nfunc (b *Base) Start() {}\n'},
]
