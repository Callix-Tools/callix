struct A { void m() {} };

int main() { A a; A* p = &a; p->m(); }
