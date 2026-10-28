def label(r):
    return r


def run(rows):
    return ", ".join(label(r) for r in rows)


run(["a"])
