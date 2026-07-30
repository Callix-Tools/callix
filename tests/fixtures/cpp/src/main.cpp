#include <curl/curl.h>

#include <iostream>
#include <string>

#include "engine.hpp"

namespace {

int fetch_one(fixture::Identifier ident)
{
    CURL *handle = curl_easy_init();
    const std::string url = "/engines/" + std::to_string(ident);

    curl_easy_setopt(handle, CURLOPT_URL, url.c_str());
    curl_easy_perform(handle);
    curl_easy_cleanup(handle);
    return 0;
}

}  // namespace

int main()
{
    const fixture::Engine engine{fixture::Region::Europe};
    const fixture::Holder<int> retries{3};

    auto report = [&engine](const fixture::Labeled &item) {
        std::cout << item.describe() << ' ' << engine.describe() << '\n';
    };

    if (engine.ping(retries.get())) {
        report(engine);
    }
    return fetch_one(1);
}
