# buffered typed call on a noise name; another package's same-named type gains the method in a later run
FILES = {
 'go.mod': 'module example.com/app\n',
 'a/server.go': 'package a\n\ntype Server struct{}\n',
 'a/run.go': 'package a\n\nfunc (s *Server) Run() { s.next() }\n',
 'c/other.go': 'package c\n\ntype Other struct{}\n\nfunc (o *Other) next() {}\n',
 'b/doc.go': 'package b\n',
}
EDITS = [
 {'b/server.go': 'package b\n\ntype Server struct{}\n\nfunc (s *Server) next() {}\n'},
]
