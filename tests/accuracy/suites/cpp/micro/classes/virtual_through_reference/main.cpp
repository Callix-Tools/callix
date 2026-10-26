struct Shape { virtual int area() = 0; };
struct Sq : Shape { int area() override { return 1; } };

int use(Shape& s) { return s.area(); }

int main() { Sq q; return use(q); }
