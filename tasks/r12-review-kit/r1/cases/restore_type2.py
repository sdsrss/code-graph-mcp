# The type and method sit in different files; the method's file is edited.
FILES = {
 'go.mod': 'module example.com/app\n',
 'b/types.go': 'package b\n\ntype Context struct{}\n',
 'b/x.go': 'package b\n\ntype X struct{}\n\nfunc (x *X) Context() int { return 0 }\n',
 'a/a.go': 'package a\n\nfunc Use(v *Thing) { v.Request.Context() }\n',
}
EDITS = [
 {'b/x.go': 'package b\n\ntype X struct{}\n\nfunc (x *X) Context() int { return 1 }\n'},
 {'b/types.go': 'package b\n\ntype Context struct{ n int }\n'},
 {'b/x.go': None},
 {'b/x.go': 'package b\n\ntype X struct{}\n\nfunc (x *X) Context() int { return 0 }\n'},
]
