/* An API macro that expands to a storage keyword. */
#ifndef FIXTURE_API_H
#define FIXTURE_API_H

#define FIXTURE_API extern

FIXTURE_API int (engine_version)(void);
FIXTURE_API int (*engine_hook)(int);

#endif /* FIXTURE_API_H */
