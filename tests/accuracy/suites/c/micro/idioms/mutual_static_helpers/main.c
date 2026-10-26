static int a(int n);
static int b(int n) { return n ? a(n - 1) : 0; }
static int a(int n) { return n ? b(n - 1) : 1; }

int main(void) { return a(3); }
