# shrunk: drop step 1 entirely
FILES = {
 'go.mod': 'module example.com/app\n',
 'b/b1.go': 'package b\n\nfunc (b Bar) Use3() { b.Foo() }\n',
 'a/a3.go': 'package a\n\ntype Foo struct{}\nfunc (t *T2) Foo() {}\n',
 'a/a2.go': 'package a\n\ntype Bar struct{}\n',
}
EDITS = [
 {'c/c1.go': 'package c\n\nfunc (t T3) Foo() {}\n'},
]
