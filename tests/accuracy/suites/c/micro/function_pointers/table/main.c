void a(void) {}
void b(void) {}
void (*table[])(void) = { a, b };

int main(void) { table[0](); return 0; }
