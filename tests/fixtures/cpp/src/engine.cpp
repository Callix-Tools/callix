#include "engine.hpp"

namespace fixture {

std::string Base::describe() const
{
    return label_;
}

Engine::Engine(Region region) : region_(region)
{
    label_ = detail::label_for(region);
}

bool Engine::ping() const
{
    return ping(1);
}

bool Engine::ping(int retries) const
{
    return retries > 0;
}

namespace detail {

std::string label_for(Region region)
{
    return region == Region::Europe ? "eu" : "us";
}

}  // namespace detail

}  // namespace fixture
