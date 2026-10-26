int a(void) { return 1; }
int b(void) { return 2; }
static int (*const table[])(void) = { a, b };

int main(void) { return table[0](); }
