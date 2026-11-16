FILES = {
 'go.mod': 'module example.com/app\n',
 'b/b1.go': 'package b\n\ntype T2 struct{}\nfunc (b Bar) Use3() { b.Foo(); b.Bar() }\n',
 'a/a3.go': 'package a\n\ntype T1 struct{}\ntype Foo int\nfunc (t *T2) Foo() {}\ntype Foo struct{}\n',
 'a/a2.go': 'package a\n\ntype Bar struct{}\nfunc Foo() {}\n',
}
EDITS = [
 {'c/c1.go': 'package c\n\ntype T1 struct{ T2 }\nfunc (t *T1) Bar() {}\nfunc Foo() {}\n'},
 {'c/c1.go': 'package c\n\nfunc (t T3) Foo() {}\n'},
]
