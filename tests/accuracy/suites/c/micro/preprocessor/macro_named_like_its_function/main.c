int f(int x) { return x; }
#define f(x) f((x) + 1)

int main(void) { return f(1); }
