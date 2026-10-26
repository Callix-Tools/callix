struct A { static void make() {} };

void f() { A::make(); }
int main() { f(); }
