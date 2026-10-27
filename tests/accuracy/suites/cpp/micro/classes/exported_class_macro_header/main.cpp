#include "export.h"

class EXPORT A {
 public:
  void m();
  int x GUARDED_BY(mu);
  int mu;
};

void A::m() {}

int main() {
  A a;
  a.m();
}
