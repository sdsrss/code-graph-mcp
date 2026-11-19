CASES = {'D02 no call inside': {'m.py': 'def deco(f):\n'
                                '    return f\n'
                                '\n'
                                '\n'
                                '@deco\n'
                                'def g():\n'
                                '    def deco():\n'
                                '        pass\n'
                                '    return 1\n'}}
