FILES = {
 'go.mod': 'module example.com/app\n',
 'a.go': '''package app

type Context struct{}
type Client struct{}

func (c *Context) Next() {}
func (c *Client) Next() {}
func (c *Client) Get() {}

func (c *Context) Handle() { c.Next() }

var x = [}

func (c *Client) Do() {
\tc.Get()
\tc.Next()
}
''',
}
