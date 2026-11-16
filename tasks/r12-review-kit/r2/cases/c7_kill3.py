# same, the other package's type already exists and only gains the method
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.next() }\n',
 'c/other.go': 'package c\n\ntype Other struct{}\n\nfunc (o *Other) next() {}\n',
 'b/server.go': 'package b\n\ntype Server struct{}\n',
}
EDITS = [
 {'b/next.go': 'package b\n\nfunc (s *Server) next() {}\n'},
]
