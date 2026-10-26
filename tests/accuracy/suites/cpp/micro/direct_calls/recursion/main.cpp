int f(int n) { return n ? f(n - 1) : 0; }

int main() { return f(3); }
