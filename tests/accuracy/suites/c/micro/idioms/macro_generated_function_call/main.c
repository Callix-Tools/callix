int f(int x) { return x; }
#define DEFINE_WRAPPER(name) int name(int x) { return f(x); }
DEFINE_WRAPPER(wrapped)

int main(void) { return wrapped(1); }
