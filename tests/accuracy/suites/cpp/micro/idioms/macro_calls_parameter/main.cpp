void run(int x) {}
#define APPLY(fn, x) fn(x)

int main() { APPLY(run, 1); return 0; }
