struct R {
    ~R() {}
};

void use() {
    R r;
}

int main() { use(); return 0; }
