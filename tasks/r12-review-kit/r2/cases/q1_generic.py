FILES = {
 'go.mod': 'module example.com/app\n',
 'a/stack.go': 'package a\n\ntype Stack[T any] struct{ xs []T }\n\nfunc (s *Stack[T]) grow() {}\n\nfunc (s *Stack[T]) Push(v T) { s.grow() }\n',
 'b/stack.go': 'package b\n\ntype Stack[T any] struct{}\n\nfunc (s *Stack[T]) grow() {}\n',
 'a/use.go': 'package a\n\nfunc (s *Stack[T]) Pop() { s.grow() }\n',
}
