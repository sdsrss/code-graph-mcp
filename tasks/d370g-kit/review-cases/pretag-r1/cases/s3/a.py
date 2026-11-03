class U:
    @staticmethod
    def fact(n):
        return 1 if n < 2 else n * U.fact(n - 1)
