#include <functional>
void g() {}

int main() { std::function<void()> f = g; f(); }
