FILES = {
 'go.mod': 'module example.com/app\n',
 'store/store.go': 'package store\n\nfunc Open() {}\n',
 'main.go': '''package main

import "example.com/app/store"

func Wire(onReady func(store string)) {
\tstore.Open()
}
''',
}
