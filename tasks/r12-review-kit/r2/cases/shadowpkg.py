FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\nfunc Load() int { return 1 }\nfunc Save() {}\n',
 'main.go': '''package main

import "example.com/app/config"

func nested(x bool) {
\tif x {
\t\tconfig := 3
\t\t_ = config
\t}
\tconfig.Load()
}

func literal() {
\tf := func(config int) { _ = config }
\tf(1)
\tconfig.Save()
}

func plain() {
\tconfig.Load()
}
''',
}
