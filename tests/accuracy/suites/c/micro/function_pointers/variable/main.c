void g(void) {}

int main(void) { void (*fp)(void) = g; fp(); return 0; }
