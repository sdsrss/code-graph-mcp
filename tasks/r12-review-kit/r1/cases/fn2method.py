# A free function becomes a method of the same name (a common refactor), in a file the caller does not touch.
FILES = {
 'go.mod': 'module example.com/app\n',
 'x/x.go': 'package x\n\ntype X struct{}\n\nfunc (x *X) Close() {}\n',
 'conn/conn.go': 'package conn\n\ntype Conn struct{}\n\nfunc Close(c *Conn) {}\n',
 'app/use.go': 'package app\n\nfunc use(c interface{ Close() }) { c.Close() }\n',
}
EDITS = [
 {'conn/conn.go': 'package conn\n\ntype Conn struct{}\n\nfunc (c *Conn) Close() {}\n'},
 {'conn/conn.go': 'package conn\n\ntype Conn struct{}\n\nfunc Close(c *Conn) {}\n'},
]
