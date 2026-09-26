// The one piece of I/O self-hosting needs that `extern fn` cannot reach on its own
// (docs/self-hosting-design.md §2): a function whose C signature already matches this language's
// own calling convention for a view parameter — pointer and length as two arguments, not the
// NUL-terminated single pointer libc's `open`/`read` expect. Declared in neant as
// `extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64;`. Linked in by a fixed convention
// (main.rs's `cc()`), not a build flag: this file, unchanged, if it exists next to the source.
//
// The caller pre-allocates `buf` at a size it chooses (the pre-sized-array discipline every
// kernel in this repo already uses) and gets the real byte count back; there is no owned-array
// return to make the type checker infer a size for, which is the gap this sidesteps rather than
// closes (an `extern` still cannot return `[T]`).

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// `write_file(path: &[u8], buf: &[u8], n: i64)`: the mirror of `read_file`, for the self-hosted
// emitter's output (docs/self-hosting-emitter-design.md §1). Returns the byte count written, or −1.
//
// Note the parameter list: a neant view passes as *two* C arguments, pointer and length, so the
// declared `buf: &[u8], n: i64` is `buf_p, buf_cap, n` here — `buf_cap` is the whole array's
// length and `n` is how much of it to write. Writing `buf_cap` instead is exactly the bug this
// signature had first, and the reason `read_file` was written to this convention rather than
// libc's in the first place.
int64_t write_file(const uint8_t *path_p, int64_t path_n, const uint8_t *buf_p, int64_t buf_cap, int64_t n) {
    if (n < 0 || n > buf_cap) return -1;
    int64_t buf_n = n;
    char pathbuf[4096];
    if (path_n < 0 || path_n >= (int64_t)sizeof(pathbuf)) return -1;
    memcpy(pathbuf, path_p, (size_t)path_n);
    pathbuf[path_n] = '\0';
    FILE *f = fopen(pathbuf, "wb");
    if (!f) return -1;
    size_t put = fwrite(buf_p, 1, (size_t)buf_n, f);
    if (fclose(f) != 0) return -1;
    return (int64_t)put;
}

int64_t read_file(const uint8_t *path_p, int64_t path_n, uint8_t *buf_p, int64_t buf_n) {
    char pathbuf[4096];
    if (path_n < 0 || path_n >= (int64_t)sizeof(pathbuf)) return -1;
    memcpy(pathbuf, path_p, (size_t)path_n);
    pathbuf[path_n] = '\0';
    FILE *f = fopen(pathbuf, "rb");
    if (!f) return -1;
    size_t got = fread(buf_p, 1, (size_t)buf_n, f);
    // a file that did not fit `buf` is not silently truncated and called a success: a short read
    // (got < buf_n) is definitely EOF; a read that exactly filled buf needs one more byte tried
    if (got == (size_t)buf_n && fgetc(f) != EOF) { fclose(f); return -1; }
    fclose(f);
    return (int64_t)got;
}

// `read_stdin(buf: &mut [u8]) -> i64` and `write_stdout(buf: &[u8], n: i64) -> i64`: the same
// convention as the pair above, over the streams instead of a named file. They exist so the
// self-hosted compiler can be an ordinary filter — `neant-self < x.nt > x.c` — rather than a
// program with its paths compiled in. That is what `compiler/main.nt` uses, and what makes seed
// two (`neant.c`) a thing you can run, not only a thing a test builds.
//
// A filter needs no argv, which is why the compiler is one. Arguments exist now (the `nt_arg`
// builtins at the end of this file, docs/decisions.md §9), for a program that calls them only.
int64_t read_stdin(uint8_t *buf_p, int64_t buf_n) {
    size_t got = fread(buf_p, 1, (size_t)buf_n, stdin);
    if (got == (size_t)buf_n && fgetc(stdin) != EOF) return -1;
    return (int64_t)got;
}

int64_t write_stdout(const uint8_t *buf_p, int64_t buf_cap, int64_t n) {
    if (n < 0 || n > buf_cap) return -1;
    size_t put = fwrite(buf_p, 1, (size_t)n, stdout);
    if (fflush(stdout) != 0) return -1;
    return (int64_t)put;
}

// `quit(code: i64)`: libc's `exit` cannot be declared directly, because an `extern fn` names the C
// symbol and neant's `i64` is `int64_t` where libc's is `int` — two declarations of `exit` that
// disagree, which `cc` refuses. One line of shim rather than an integer-width rule in the language.
void quit(int64_t code) { exit((int)code); }

// The input builtins (src/input.rs, docs/decisions.md "Program input"): `arg_count()`, `arg(k)`,
// `read_file(path)` and `file_size(path)`. Not the pair above — those are a program's own externs
// over a buffer it pre-sized — but builtins that hand back an owned `[u8]` whose length the cost
// pass takes as a size of its own. They are `nt_`-prefixed because `read_file` above already has
// the name, and the emitter calls them by that prefix. The struct is the one the emitted C
// declares for a function returning `[u8]`: the same tag and members, so the same type.
#include <errno.h>

struct nt_arr_uint8_t { uint8_t *p; int64_t n; };

static int nt_argc;
static char **nt_argv;

// called first by an emitted `main` whose program reads its arguments; `argv[0]` is the binary,
// a temporary under `neant run`, and is not one of them
void nt_set_args(int argc, char **argv) { nt_argc = argc; nt_argv = argv; }

int64_t nt_arg_count(void) { return nt_argc > 1 ? nt_argc - 1 : 0; }

static struct nt_arr_uint8_t nt_bytes(const void *src, int64_t n) {
    struct nt_arr_uint8_t a = { malloc(n == 0 ? 1 : (size_t)n), n };
    if (!a.p) { fprintf(stderr, "out of memory\n"); exit(101); }
    if (n > 0) memcpy(a.p, src, (size_t)n);
    return a;
}

// an argument that is not there is the same failure as an index out of bounds, and exits the same
struct nt_arr_uint8_t nt_arg(int64_t k) {
    int64_t n = nt_arg_count();
    if (k < 0 || k >= n) {
        fprintf(stderr, "argument %lld out of range: the program was given %lld\n", (long long)k, (long long)n);
        exit(101);
    }
    const char *s = nt_argv[k + 1];
    return nt_bytes(s, (int64_t)strlen(s));
}

static int nt_path(char *out, size_t cap, const uint8_t *p, int64_t n) {
    if (n < 0 || (size_t)n >= cap || memchr(p, 0, (size_t)n)) return -1;
    memcpy(out, p, (size_t)n);
    out[n] = '\0';
    return 0;
}

// the error value: −1 when the file cannot be opened, so a program can test before it reads
int64_t nt_file_size(const uint8_t *path_p, int64_t path_n) {
    char path[4096];
    if (nt_path(path, sizeof path, path_p, path_n) != 0) return -1;
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    int64_t n = -1;
    if (fseek(f, 0, SEEK_END) == 0) n = (int64_t)ftell(f);
    fclose(f);
    return n;
}

// the whole file, read to its end rather than to a size asked for first (a pipe has none); a file
// that cannot be read ends the program with the reason, as `nt_arg` does for a missing argument
struct nt_arr_uint8_t nt_read_file(const uint8_t *path_p, int64_t path_n) {
    char path[4096];
    FILE *f = NULL;
    if (nt_path(path, sizeof path, path_p, path_n) == 0) f = fopen(path, "rb");
    else { errno = ENAMETOOLONG; snprintf(path, sizeof path, "%.*s", (int)(path_n < 200 ? path_n : 200), (const char *)path_p); }
    if (!f) { fprintf(stderr, "cannot read %s: %s\n", path, strerror(errno)); exit(101); }
    size_t cap = 4096, n = 0;
    uint8_t *buf = malloc(cap);
    if (!buf) { fprintf(stderr, "out of memory\n"); exit(101); }
    for (;;) {
        if (n == cap) {
            cap *= 2;
            uint8_t *b = realloc(buf, cap);
            if (!b) { fprintf(stderr, "out of memory\n"); exit(101); }
            buf = b;
        }
        size_t got = fread(buf + n, 1, cap - n, f);
        n += got;
        if (got == 0) break;
    }
    if (ferror(f)) { fprintf(stderr, "cannot read %s: %s\n", path, strerror(errno)); exit(101); }
    fclose(f);
    struct nt_arr_uint8_t a = { buf, (int64_t)n };
    return a;
}

// the first `n` bytes of `s`, as they are, to stdout (docs/decisions.md §13); `n` beyond the view
// is a bounds failure, as an index past it is
void nt_print_bytes(const uint8_t *s_p, int64_t s_n, int64_t n) {
    if (n < 0 || n > s_n) { fprintf(stderr, "print_bytes of %lld bytes from a view of %lld\n", (long long)n, (long long)s_n); exit(101); }
    fwrite(s_p, 1, (size_t)n, stdout);
}
