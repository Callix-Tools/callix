int f(int x) { return x; }
#define CALL(x) f(x)

int main(void) { return CALL(1); }
