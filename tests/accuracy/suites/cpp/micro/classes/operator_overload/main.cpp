struct A { A operator+(const A&) const { return A(); } };

int main() { A a, b; a + b; }
