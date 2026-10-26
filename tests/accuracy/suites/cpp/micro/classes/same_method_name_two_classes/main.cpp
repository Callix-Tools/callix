struct A { void run() {} };
struct B { void run() {} };

void f(A& a, B& b) { a.run(); b.run(); }

int main() { A a; B b; f(a, b); }
