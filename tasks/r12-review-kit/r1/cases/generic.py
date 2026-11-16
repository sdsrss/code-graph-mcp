FILES = {
 'go.mod': 'module example.com/app\n',
 'set.go': '''package app

type Set[T comparable] struct{}

func (s *Set[T]) Add(v T) {}
func (s *Set[T]) AddAll(vs []T) { for _, v := range vs { s.Add(v) } }

type Celsius float64

func (c Celsius) String() string { return "" }
func (c Celsius) Show() string { return c.String() }

type List struct{}

func (l *List) Add(v int) {}
func (l *List) String() string { return "" }
''',
}
