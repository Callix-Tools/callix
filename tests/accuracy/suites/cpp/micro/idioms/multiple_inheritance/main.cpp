struct A { void fa() {} };
struct B { void fb() {} };
struct C : A, B {};

int main() { C c; c.fa(); c.fb(); }
