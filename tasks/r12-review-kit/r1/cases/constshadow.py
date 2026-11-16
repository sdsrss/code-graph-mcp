FILES = {
 'go.mod': 'module example.com/app\n',
 'a.go': '''package app

import "time"

type Ticker struct{}
type Dur struct{}

func (t *Ticker) String() string { return "" }
func (d Dur) String() string { return "" }

func (t *Ticker) Describe() string {
\tconst t = time.Second
\treturn t.String()
}
''',
}
