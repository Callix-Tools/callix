#define FAST 1
#if FAST
int f(void) { return 1; }
#else
int f(void) { return 2; }
#endif

int main(void) { return f(); }
