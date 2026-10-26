struct A { A() {} ~A() {} };

int main() { A* p = new A(); delete p; }
