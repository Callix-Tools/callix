struct A { A() {} };
struct B : A { B() : A() {} };

int main() { B b; }
