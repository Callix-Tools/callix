#include <thread>
void worker() {}

int main() {
    std::thread t(worker);
    t.join();
    return 0;
}
