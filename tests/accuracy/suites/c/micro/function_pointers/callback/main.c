void cb(void) {}
void run(void (*f)(void)) { f(); }

int main(void) { run(cb); return 0; }
