int f(int x) { return x; }
#define INNER(x) f(x)
#define OUTER(x) INNER(x)

int main(void) { return OUTER(1); }
