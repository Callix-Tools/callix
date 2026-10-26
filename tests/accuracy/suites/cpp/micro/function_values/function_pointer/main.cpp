void g() {}

int main() { void (*f)() = g; f(); }
