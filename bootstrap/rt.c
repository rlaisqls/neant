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
#include <string.h>

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
