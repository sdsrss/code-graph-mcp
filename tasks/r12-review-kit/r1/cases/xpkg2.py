FILES = {
 'go.mod': 'module example.com/app\n',
 'a/client.go': 'package a\n\ntype Client struct{}\n\nfunc (c *Client) Do() {}\n',
 'a/use.go': 'package a\n\nfunc (c *Client) Run() { c.Do() }\n',
 'b/client.go': 'package b\n\ntype Client struct{}\n\nfunc (c *Client) Do() {}\n',
}
