step_1() { printf 'def f():\n    return 1\n' > pkg/newmod.py; }
step_2() { printf 'def f():\n    return 1\n\n\ndef h():\n    return 2\n' > pkg/newmod.py; }
