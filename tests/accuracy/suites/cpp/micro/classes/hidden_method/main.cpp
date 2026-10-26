struct A { void m() {} };
struct B : A { void m() {} };

int main() { B b; b.m(); }
