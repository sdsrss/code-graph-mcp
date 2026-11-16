FILES = {
 'go.mod': 'module example.com/app\n',
 'b/b.go': 'package b\n\ntype X struct{}\n\nfunc (x *X) Context() int { return 0 }\n',
 'a/a.go': 'package a\n\nfunc Use(v *Thing) { v.Request.Context() }\n\nfunc Use2(w *Thing) { w.Context() }\n',
}
EDITS = [
 {'b/b.go': 'package b\n\ntype X struct{}\n\ntype Context struct{}\n\nfunc (x *X) Context() int { return 0 }\n'},
 {'b/b.go': 'package b\n\ntype X struct{}\n\ntype Context struct{}\n\nfunc (x *X) Context() int { return 1 }\nfunc helper() {}\n'},
 {'b/b.go': 'package b\n\ntype X struct{}\n\nfunc (x *X) Context() int { return 0 }\n'},
]
