typedef void (*handler)(void);
void g(void) {}

int main(void) { handler h = g; h(); return 0; }
