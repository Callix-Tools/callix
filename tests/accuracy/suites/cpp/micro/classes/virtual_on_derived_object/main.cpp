struct Shape { virtual int area() { return 0; } };
struct Sq : Shape { int area() override { return 1; } };

int main() { Sq q; return q.area(); }
