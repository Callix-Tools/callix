int v(void) { return 1; }
int f(int x) { return x; }

int main(void) { return f(v()); }
