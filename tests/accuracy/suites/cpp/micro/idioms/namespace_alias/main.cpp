namespace very { namespace long_name { void f() {} } }
namespace m = very::long_name;

int main() { m::f(); }
