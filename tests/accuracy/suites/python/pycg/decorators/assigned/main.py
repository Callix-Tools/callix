def dec(f):
    return f

a = dec

@a
def func():
    pass

func()
