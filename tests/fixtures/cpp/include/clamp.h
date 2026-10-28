#pragma once
namespace fixture {
inline int clamp(int v) { return v < 0 ? 0 : v; }
}
