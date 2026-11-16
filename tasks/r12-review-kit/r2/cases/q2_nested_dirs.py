FILES = {
 'go.mod': 'module example.com/app\n',
 'server.go': 'package app\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
 'run.go': 'package app\n\nfunc (s *Server) Run() { s.Start() }\n',
 'sub/server.go': 'package sub\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
 'sub/run.go': 'package sub\n\nfunc (s *Server) Run() { s.Start() }\n',
 'sub/deep/server.go': 'package deep\n\ntype Server struct{}\n\nfunc (s *Server) Start() {}\n',
 'sub/deep/run.go': 'package deep\n\nfunc (s *Server) Run() { s.Start() }\n',
}
