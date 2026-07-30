// Structured-exception guard.
//
// Rust (even on MSVC) has no __try/__except and std::panic cannot catch a hardware
// fault such as an access violation. The whole engine pokes raw memory at offsets
// that may be wrong after a game patch, so we need the same safety net the C++
// version had: run a callback under __try and report whether it faulted, instead of
// taking the whole game down. src/seh.rs wraps this behind a safe `guard(|| ...)`.

typedef void (*huragok_cb)(void *);

// Returns 1 if fn(ctx) completed, 0 if it raised a structured exception.
int huragok_seh_try(huragok_cb fn, void *ctx)
{
    __try
    {
        fn(ctx);
        return 1;
    }
    __except (1 /* EXCEPTION_EXECUTE_HANDLER */)
    {
        return 0;
    }
}
