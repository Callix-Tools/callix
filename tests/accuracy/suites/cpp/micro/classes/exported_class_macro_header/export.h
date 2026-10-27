#if !defined(EXPORT)
#if defined(SHARED_LIBRARY)
#if defined(_WIN32)
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif
#else
#define EXPORT
#endif
#endif

#define ATTR(x)  // no-op
#define GUARDED_BY(x) ATTR(guarded_by(x))
