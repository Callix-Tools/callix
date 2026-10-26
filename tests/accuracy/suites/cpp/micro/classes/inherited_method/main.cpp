struct A { void m() {} };
struct B : A {};

int main() { B b; b.m(); }
