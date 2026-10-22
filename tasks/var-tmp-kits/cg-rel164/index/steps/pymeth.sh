step_1() { printf '\n\nclass Box:\n    def helper(self):\n        return 2\n\n\ndef other():\n    return 0\n' > pkg/util.py; }
step_2() { printf 'from pkg import helper\n\n\ndef main():\n    return helper() + 1\n' > app.py; }
