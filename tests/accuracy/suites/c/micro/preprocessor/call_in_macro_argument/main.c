int f(int x) { return x; }
#define TWICE(x) ((x) + (x))

int main(void) { return TWICE(f(1)); }
