#include <algorithm>
int twice(int x) { return 2 * x; }

int main() {
    int a[3] = {1, 2, 3};
    int out[3];
    std::transform(a, a + 3, out, twice);
    return 0;
}
