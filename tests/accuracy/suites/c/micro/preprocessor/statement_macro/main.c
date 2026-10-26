int f(int x) { return x; }
int g(int x) { return x; }
#define RUN(x) do { f(x); g(x); } while (0)

int main(void) { RUN(1); return 0; }
