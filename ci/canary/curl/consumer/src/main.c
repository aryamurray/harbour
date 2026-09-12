/* A consumer of the libcurl Harbour built from probe answers.
 *
 * The job of this program is to fail loudly if any of the 108 measured
 * answers was wrong, and "it linked" is not enough for that: curl is written
 * so that a wrong `HAVE_*` produces a library that links and then misbehaves
 * at runtime. So this drives the parts of curl whose behaviour depends on
 * the config header and checks the results.
 *
 * No TLS, and no network. `curl_easy_perform` on a `file://` URL is a real
 * transfer through curl's full machinery -- URL parsing, protocol dispatch,
 * the connection-filter chain, the buffered writer, the progress meter --
 * and it touches none of the code paths a missing TLS backend would.
 *
 * Which config answers each check depends on is named, because a check whose
 * failure does not point at anything is a check nobody can act on.
 */
#include <curl/curl.h>

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

/* Informational output is buffered and printed *after* the `OK` line.
 *
 * `ci/canary/lib.sh` requires the consumer's output to *begin* with `OK`,
 * which is a deliberately strict check -- it is what makes "the consumer
 * printed something reassuring somewhere in its output" not count. The
 * detail is still worth having in the log, so it is collected and flushed at
 * the end rather than dropped.
 */
static char notes[4096];
static size_t notes_len = 0;

static void note(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(notes + notes_len, sizeof(notes) - notes_len, fmt, ap);
    va_end(ap);
    if (n > 0 && (size_t) n < sizeof(notes) - notes_len)
        notes_len += (size_t) n;
}

#define CHECK(cond, what)                                                     \
    do {                                                                      \
        if (!(cond)) {                                                        \
            printf("FAIL %s\n", (what));                                      \
            failures++;                                                       \
        }                                                                     \
    } while (0)

#define CHECK_OK(rc, what)                                                    \
    do {                                                                      \
        CURLcode rc_ = (rc);                                                  \
        if (rc_ != CURLE_OK) {                                                \
            printf("FAIL %s: %s\n", (what), curl_easy_strerror(rc_));         \
            failures++;                                                       \
        }                                                                     \
    } while (0)

struct sink {
    char buf[4096];
    size_t len;
};

static size_t collect(char *data, size_t size, size_t nmemb, void *userp)
{
    struct sink *s = (struct sink *) userp;
    size_t n = size * nmemb;
    if (s->len + n >= sizeof(s->buf))
        return 0; /* Telling curl to abort is itself a code path. */
    memcpy(s->buf + s->len, data, n);
    s->len += n;
    s->buf[s->len] = '\0';
    return n;
}

/* The payload. Deliberately larger than one write callback's worth of a
 * trivial file, so the chunked writer is exercised rather than a single
 * memcpy. */
#define LINE "the quick brown fox jumps over the lazy dog\n"

static void check_version_info(void)
{
    curl_version_info_data *v = curl_version_info(CURLVERSION_NOW);
    CHECK(v != NULL, "curl_version_info returned NULL");
    if (!v)
        return;

    /* `version` comes from curlver.h, not from the config header, so this is
     * a check that we linked the library we think we did. */
    CHECK(strcmp(v->version, "8.22.0") == 0, "libcurl is not 8.22.0");
    note("   libcurl %s, host %s\n", v->version, v->host);

    /* CURL_VERSION_IPV6 is set iff USE_IPV6 was defined, which is a literal
     * in the manifest. */
    CHECK((v->features & CURL_VERSION_IPV6) != 0,
          "USE_IPV6 did not reach the build (no IPv6 feature bit)");
    /* CURL_VERSION_UNIX_SOCKETS iff USE_UNIX_SOCKETS. */
    CHECK((v->features & CURL_VERSION_UNIX_SOCKETS) != 0,
          "USE_UNIX_SOCKETS did not reach the build");
    /* CURL_VERSION_THREADSAFE iff HAVE_ATOMIC or HAVE_THREADS_POSIX. */
    CHECK((v->features & CURL_VERSION_THREADSAFE) != 0,
          "neither HAVE_ATOMIC nor HAVE_THREADS_POSIX reached the build");
    /* And the negative: no TLS backend was configured, so there must be no
     * SSL feature bit. A canary that only checks for things being present
     * cannot tell a correct config from one that turned everything on. */
    CHECK((v->features & CURL_VERSION_SSL) == 0,
          "an SSL backend appeared in a build configured without one");

    /* `file` must be in the protocol list -- the transfer below depends on
     * it -- and `ldap` must not, because CURL_DISABLE_LDAP is a literal
     * define in the manifest. Both halves matter. */
    int have_file = 0, have_ldap = 0;
    for (const char *const *p = v->protocols; p && *p; p++) {
        if (strcmp(*p, "file") == 0)
            have_file = 1;
        if (strcmp(*p, "ldap") == 0)
            have_ldap = 1;
    }
    CHECK(have_file, "the `file` protocol is missing");
    CHECK(!have_ldap, "CURL_DISABLE_LDAP did not reach the build");
}

/* `curl_off_t` arithmetic, which is what SIZEOF_CURL_OFF_T is for.
 *
 * curl's `CURL_FORMAT_CURL_OFF_T` and its internal 64-bit parsing are
 * selected by that size, and a wrong answer there is the classic way a
 * configure mistake becomes a silent truncation rather than a build error.
 * SIZEOF_CURL_OFF_T is measured by a probe against curl's *own* typedef. */
static void check_off_t_width(void)
{
    CHECK(sizeof(curl_off_t) == 8,
          "curl_off_t is not 64-bit, so SIZEOF_CURL_OFF_T is wrong");

    /* A value that does not fit in 32 bits, round-tripped through curl's own
     * printf implementation (mprintf.c, which reads SIZEOF_CURL_OFF_T). */
    curl_off_t big = (curl_off_t) 5000000000LL;
    char *printed = curl_maprintf("%" CURL_FORMAT_CURL_OFF_T, big);
    CHECK(printed != NULL, "curl_maprintf returned NULL");
    if (printed) {
        CHECK(strcmp(printed, "5000000000") == 0,
              "curl's printf truncated a 64-bit offset -- SIZEOF_CURL_OFF_T "
              "or CURL_FORMAT_CURL_OFF_T is wrong");
        curl_free(printed);
    }
}

/* The URL API, which is a large amount of pure parsing with no I/O. */
static void check_url_api(void)
{
    CURLU *u = curl_url();
    CHECK(u != NULL, "curl_url returned NULL");
    if (!u)
        return;

    CURLUcode uc = curl_url_set(
        u, CURLUPART_URL, "http://user:pw@example.com:8080/a/b?x=1#frag", 0);
    CHECK(uc == CURLUE_OK, "curl_url_set rejected a valid URL");

    char *part = NULL;
    if (curl_url_get(u, CURLUPART_HOST, &part, 0) == CURLUE_OK) {
        CHECK(strcmp(part, "example.com") == 0, "wrong host parsed");
        curl_free(part);
    } else {
        CHECK(0, "curl_url_get(HOST) failed");
    }
    if (curl_url_get(u, CURLUPART_PORT, &part, 0) == CURLUE_OK) {
        CHECK(strcmp(part, "8080") == 0, "wrong port parsed");
        curl_free(part);
    } else {
        CHECK(0, "curl_url_get(PORT) failed");
    }
    if (curl_url_get(u, CURLUPART_QUERY, &part, 0) == CURLUE_OK) {
        CHECK(strcmp(part, "x=1") == 0, "wrong query parsed");
        curl_free(part);
    } else {
        CHECK(0, "curl_url_get(QUERY) failed");
    }

    /* Relative resolution against the URL already set, which is the code
     * path a redirect takes. */
    uc = curl_url_set(u, CURLUPART_URL, "../c?y=2", 0);
    CHECK(uc == CURLUE_OK, "curl_url_set rejected a relative URL");
    if (curl_url_get(u, CURLUPART_URL, &part, 0) == CURLUE_OK) {
        CHECK(strcmp(part, "http://user:pw@example.com:8080/c?y=2") == 0,
              "relative URL resolution produced the wrong result");
        curl_free(part);
    } else {
        CHECK(0, "curl_url_get(URL) failed after a relative set");
    }

    curl_url_cleanup(u);

    /* And a URL that must be *rejected*. On a fresh handle, deliberately:
     * `curl_url_set(CURLUPART_URL)` on a handle that already holds a URL
     * performs relative resolution instead, so the first version of this
     * check reused the handle above and "not a url at all" was accepted as a
     * relative path. A negative assertion that cannot fail is worse than no
     * assertion. */
    CURLU *fresh = curl_url();
    CHECK(fresh != NULL, "curl_url returned NULL");
    if (fresh) {
        CHECK(curl_url_set(fresh, CURLUPART_URL, "not a url at all", 0)
                  == CURLUE_MALFORMED_INPUT,
              "curl_url_set accepted nonsense");
        CHECK(curl_url_set(fresh, CURLUPART_URL, "http://[::1", 0)
                  == CURLUE_BAD_IPV6,
              "curl_url_set accepted a truncated IPv6 literal");
        curl_url_cleanup(fresh);
    }
}

/* Escaping, and curl's own string/base64 helpers under `lib/curlx/`. */
static void check_escape(void)
{
    CURL *e = curl_easy_init();
    CHECK(e != NULL, "curl_easy_init returned NULL");
    if (!e)
        return;

    char *esc = curl_easy_escape(e, "a b/c?d", 7);
    CHECK(esc != NULL, "curl_easy_escape returned NULL");
    if (esc) {
        CHECK(strcmp(esc, "a%20b%2Fc%3Fd") == 0, "wrong percent-encoding");
        int outlen = 0;
        char *un = curl_easy_unescape(e, esc, 0, &outlen);
        CHECK(un != NULL && outlen == 7 && memcmp(un, "a b/c?d", 7) == 0,
              "percent-decoding did not round-trip");
        curl_free(un);
        curl_free(esc);
    }
    curl_easy_cleanup(e);
}

/* A real transfer. This is the check that "it linked" cannot substitute for.
 *
 * Depends, among other things, on HAVE_FCNTL_O_NONBLOCK (curl sets the
 * transfer's descriptors non-blocking through `curlx/nonblock.c`, which
 * picks its mechanism from the config header), on the `select`/`poll`
 * answers, and on SIZEOF_CURL_OFF_T for the content-length bookkeeping. */
static void check_file_transfer(const char *path)
{
    char url[2048];
    snprintf(url, sizeof(url), "file://%s", path);

    struct sink got;
    memset(&got, 0, sizeof(got));

    CURL *e = curl_easy_init();
    CHECK(e != NULL, "curl_easy_init returned NULL");
    if (!e)
        return;

    CHECK_OK(curl_easy_setopt(e, CURLOPT_URL, url), "setopt URL");
    CHECK_OK(curl_easy_setopt(e, CURLOPT_WRITEFUNCTION, collect),
             "setopt WRITEFUNCTION");
    CHECK_OK(curl_easy_setopt(e, CURLOPT_WRITEDATA, &got), "setopt WRITEDATA");
    /* Two options whose *acceptance* depends on the config: NOSIGNAL exists
     * always, but TIMEOUT_MS goes through curl's millisecond clock, which is
     * HAVE_CLOCK_GETTIME_MONOTONIC / HAVE_MACH_ABSOLUTE_TIME. */
    CHECK_OK(curl_easy_setopt(e, CURLOPT_NOSIGNAL, 1L), "setopt NOSIGNAL");
    CHECK_OK(curl_easy_setopt(e, CURLOPT_TIMEOUT_MS, 10000L), "setopt TIMEOUT_MS");
    CHECK_OK(curl_easy_setopt(e, CURLOPT_FOLLOWLOCATION, 1L),
             "setopt FOLLOWLOCATION");
    /* An option that must be *rejected*, because its feature was compiled
     * out. `CURLOPT_HSTS` is behind CURL_DISABLE_HSTS, a literal define in
     * the manifest -- so this asserts a define reached the build by the
     * absence of a capability, which is the harder direction. */
    CHECK(curl_easy_setopt(e, CURLOPT_HSTS, "/dev/null") != CURLE_OK,
          "CURLOPT_HSTS was accepted, so CURL_DISABLE_HSTS did not reach the "
          "build");

    CHECK_OK(curl_easy_perform(e), "curl_easy_perform on a file:// URL");

    size_t want = strlen(LINE) * 64;
    CHECK(got.len == want, "the transfer returned the wrong number of bytes");
    if (got.len == want) {
        int same = 1;
        for (size_t i = 0; i < 64; i++)
            if (memcmp(got.buf + i * strlen(LINE), LINE, strlen(LINE)) != 0)
                same = 0;
        CHECK(same, "the transferred bytes are not what was written");
    }

    /* `getinfo` reads curl's own bookkeeping back out. SPEED_DOWNLOAD_T and
     * SIZE_DOWNLOAD_T are `curl_off_t`, so a wrong SIZEOF_CURL_OFF_T shows
     * up here as a garbage number rather than as a compile error. */
    curl_off_t dl = -1;
    CHECK_OK(curl_easy_getinfo(e, CURLINFO_SIZE_DOWNLOAD_T, &dl),
             "getinfo SIZE_DOWNLOAD_T");
    CHECK(dl == (curl_off_t) want,
          "CURLINFO_SIZE_DOWNLOAD_T disagrees with the bytes received");

    long code = 0;
    CHECK_OK(curl_easy_getinfo(e, CURLINFO_RESPONSE_CODE, &code),
             "getinfo RESPONSE_CODE");

    curl_easy_cleanup(e);
    note("   transferred %lu bytes over file://\n", (unsigned long) got.len);
}

/* The multi interface, driven to completion. A different scheduler, the same
 * transfer, and the part of curl that leans hardest on the poll/select
 * answers in the config header. */
static void check_multi_transfer(const char *path)
{
    char url[2048];
    snprintf(url, sizeof(url), "file://%s", path);

    struct sink got;
    memset(&got, 0, sizeof(got));

    CURLM *m = curl_multi_init();
    CURL *e = curl_easy_init();
    CHECK(m != NULL && e != NULL, "curl_multi_init / curl_easy_init failed");
    if (!m || !e)
        return;

    curl_easy_setopt(e, CURLOPT_URL, url);
    curl_easy_setopt(e, CURLOPT_WRITEFUNCTION, collect);
    curl_easy_setopt(e, CURLOPT_WRITEDATA, &got);
    CHECK(curl_multi_add_handle(m, e) == CURLM_OK, "curl_multi_add_handle");

    int running = 1;
    int spins = 0;
    while (running && spins++ < 10000) {
        CURLMcode mc = curl_multi_perform(m, &running);
        CHECK(mc == CURLM_OK, "curl_multi_perform");
        if (mc != CURLM_OK)
            break;
        if (running) {
            int numfds = 0;
            /* `curl_multi_poll` is the thing that needs poll(2) -- which is
             * HAVE_POLL and HAVE_POLL_H from the config header. */
            mc = curl_multi_poll(m, NULL, 0, 50, &numfds);
            CHECK(mc == CURLM_OK, "curl_multi_poll");
            if (mc != CURLM_OK)
                break;
        }
    }
    CHECK(spins < 10000, "curl_multi_perform never finished");

    int msgs = 0;
    CURLMsg *msg = curl_multi_info_read(m, &msgs);
    CHECK(msg != NULL, "no message from curl_multi_info_read");
    if (msg) {
        CHECK(msg->msg == CURLMSG_DONE, "the multi transfer did not finish");
        CHECK_OK(msg->data.result, "the multi transfer failed");
    }
    CHECK(got.len == strlen(LINE) * 64,
          "the multi transfer returned the wrong number of bytes");

    curl_multi_remove_handle(m, e);
    curl_easy_cleanup(e);
    curl_multi_cleanup(m);
    note("   multi interface transferred %lu bytes\n",
         (unsigned long) got.len);
}

int main(void)
{
    CHECK_OK(curl_global_init(CURL_GLOBAL_DEFAULT), "curl_global_init");

    /* Write the payload somewhere curl can fetch it. `tmpnam` is deprecated
     * and `mkstemp` needs a header the config header does not decide, so
     * this uses a fixed name next to the binary's working directory. */
    const char *path = "curl-canary-payload.txt";
    FILE *f = fopen(path, "wb");
    CHECK(f != NULL, "could not write the payload file");
    if (f) {
        for (int i = 0; i < 64; i++)
            fputs(LINE, f);
        fclose(f);
    }

    char abspath[4096];
    if (!realpath(path, abspath)) {
        printf("FAIL could not resolve the payload path\n");
        failures++;
        abspath[0] = '\0';
    }

    check_version_info();
    check_off_t_width();
    check_url_api();
    check_escape();
    if (abspath[0]) {
        check_file_transfer(abspath);
        check_multi_transfer(abspath);
    }

    remove(path);
    curl_global_cleanup();

    if (failures) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("OK curl 8.22.0: version info, curl_off_t width, URL API, "
           "escaping, easy and multi file:// transfers\n");
    fputs(notes, stdout);
    return 0;
}
