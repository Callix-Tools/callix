int g(int x) { return x; }
#define APPLY(fn, x) fn(x)

int main(void) { return APPLY(g, 1); }
