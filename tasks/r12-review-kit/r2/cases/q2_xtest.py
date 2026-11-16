# package x_test in the same directory declares its own T with Close
FILES = {
 'go.mod': 'module example.com/app\n',
 'x/x.go': 'package x\n\ntype T struct{}\n\nfunc (t *T) Close() {}\n',
 'x/helpers_test.go': 'package x_test\n\ntype T struct{}\n\nfunc (t *T) Close() {}\n',
 'x/x_test.go': 'package x_test\n\nfunc (t *T) Run() { t.Close() }\n',
 'x/y.go': 'package x\n\nfunc (t *T) Run() { t.Close() }\n',
}
