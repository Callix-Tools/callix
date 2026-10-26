class Box {
    map(f: () => void) {}
}
function cb() {}
new Box().map(cb);
