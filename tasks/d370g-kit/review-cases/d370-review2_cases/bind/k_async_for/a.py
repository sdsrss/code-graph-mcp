class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


import asyncio
async def h():
    global C
    async def it():
        yield D
    async for C in it():
        pass
asyncio.run(h())


def t_call():
    return C.m(None)
