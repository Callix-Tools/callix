#include <algorithm>

bool less_than(int a, int b) { return a < b; }

int main() {
    int x[2] = {2, 1};
    std::sort(x, x + 2, less_than);
}
