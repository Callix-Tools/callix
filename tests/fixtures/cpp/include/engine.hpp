// Declarations shared by both translation units.
#pragma once

#include <string>
#include <vector>

namespace fixture {

enum class Region { Europe, America };

using Identifier = long;

// Abstract base: Engine inherits through Base.
struct Labeled {
    virtual ~Labeled() = default;
    virtual std::string describe() const = 0;
};

template <typename T>
class Holder {
public:
    explicit Holder(T value) : value_(value) {}
    T get() const { return value_; }

private:
    T value_;
};

class Base : public Labeled {
public:
    static constexpr const char *DefaultLabel = "base";

    std::string describe() const override;

protected:
    std::string label_ = DefaultLabel;
};

class Engine final : public Base {
public:
    explicit Engine(Region region);

    // Overloads: same name, different parameters.
    bool ping() const;
    bool ping(int retries) const;

    Region region() const { return region_; }

private:
    Region region_;
};

namespace detail {
std::string label_for(Region region);
}  // namespace detail

}  // namespace fixture
