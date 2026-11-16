FILES = {
 'go.mod': 'module example.com/app\n',
 'a.go': '''package app

type Context struct{}

func (c *Context) Next() {}

var handler = func(c *Other) { c.Next() }

type Other struct{}

func (o *Other) Next() {}

func init() { handler(nil) }
''',
}
