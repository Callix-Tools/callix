#include <curl/curl.h>
#include <stdio.h>

#include "engine.h"

/* Same name as engine.c's static helper, and a different function. */
static const char *label_for(enum region region)
{
    return region == REGION_AMERICA ? "america" : "europe";
}

static int fetch_one(long ident)
{
    CURL *handle = curl_easy_init();
    char url[64];

    snprintf(url, sizeof(url), "/engines/%ld", ident);
    curl_easy_setopt(handle, CURLOPT_URL, url);
    curl_easy_perform(handle);
    curl_easy_cleanup(handle);
    return 0;
}

int main(void)
{
    engine_t engine = engine_build(REGION_EUROPE);

    if (engine_ping(&engine)) {
        printf("%s %s\n", engine_describe(&engine), label_for(engine.region));
    }
    return fetch_one(1);
}
