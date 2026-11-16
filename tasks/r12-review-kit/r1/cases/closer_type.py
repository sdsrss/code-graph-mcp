FILES = {
 'go.mod': 'module example.com/app\n',
 'far/x.go': 'package far\n\ntype X struct{}\n\nfunc (x *X) Render() {}\n',
 'app/use.go': 'package app\n\nfunc Use(v *Thing) { v.Out.Render() }\n',
 'app/types.go': 'package app\n\ntype Thing struct{}\n',
}
EDITS = [
 {'app/types.go': 'package app\n\ntype Thing struct{}\n\ntype Render interface{ Do() }\n'},
 {'app/types.go': 'package app\n\ntype Thing struct{}\n'},
]
