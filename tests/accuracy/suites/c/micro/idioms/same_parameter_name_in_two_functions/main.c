void g(void) {}
void a(void (*cb)(void)) { cb(); }
void b(void (*cb)(void)) { cb(); }

int main(void) { a(g); return 0; }
