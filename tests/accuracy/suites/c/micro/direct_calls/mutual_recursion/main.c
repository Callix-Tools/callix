int odd(int n);
int even(int n) { return n ? odd(n - 1) : 1; }
int odd(int n) { return n ? even(n - 1) : 0; }

int main(void) { return even(4); }
