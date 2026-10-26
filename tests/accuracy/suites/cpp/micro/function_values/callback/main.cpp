void cb() {}
void run(void (*f)()) { f(); }

int main() { run(cb); }
