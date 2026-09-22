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
// The language has no argv, and giving it one would mean the emitted `main` taking arguments and
// a way to pass them across a translation unit for every program, linked or not. A filter needs
// neither, and is the convention the input already has.
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
